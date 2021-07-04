use async_executor::Executor;
use async_io::Async;
use async_lock::{Mutex, MutexGuard};
use futures_lite::{future, AsyncReadExt, AsyncWriteExt};
use linefeed::{Interface, ReadResult};
use log::{error, info, warn};
use quartz::{
    network::{
        packet::{ClientBoundPacket, ServerBoundPacket},
        ConnectionState,
        PacketBuffer,
        PacketSerdeError,
        LEGACY_PING_PACKET_ID,
        PROTOCOL_VERSION,
    },
    util::logging,
};
use std::{
    error::Error,
    fmt::{Debug, Display},
    net::{Shutdown, TcpListener, TcpStream},
    sync::Arc,
    thread,
};

static EXECUTOR: Executor = Executor::new();

fn main() -> Result<(), Box<dyn Error>> {
    let console_interface = Arc::new(Interface::new("quartz-proxy")?);
    console_interface.set_prompt("> ")?;

    logging::init_logger(None, console_interface.clone())?;

    let (shutdown_tx, shutdown_rx) = async_channel::bounded::<()>(1);

    for i in 1 ..= 4 {
        let shutdown_rx_clone = shutdown_rx.clone();

        thread::Builder::new()
            .name(format!("quartz_proxy#{}", i))
            .spawn(move || future::block_on(EXECUTOR.run(shutdown_rx_clone.recv())))?;
    }

    let proxy_server = Async::new(TcpListener::bind("0.0.0.0:25566")?)?;
    EXECUTOR
        .spawn(async move {
            if let Err(e) = handle_incoming_connection(proxy_server).await {
                error!("{}", e);
            }
        })
        .detach();

    loop {
        match console_interface.read_line() {
            Ok(result) => match result {
                ReadResult::Input(command) => {
                    console_interface.add_history_unique(command.clone());

                    match command.as_str() {
                        "stop" => break,
                        _ => println!("Invalid command"),
                    }
                }
                _ => {}
            },
            Err(e) => {
                error!("Failed to read console input: {}", e);
                break;
            }
        }
    }

    let _ = future::block_on(shutdown_tx.send(()));
    logging::cleanup();
    println!();
    Ok(())
}

async fn handle_incoming_connection(server: Async<TcpListener>) -> Result<(), Box<dyn Error>> {
    loop {
        match server.accept().await {
            // Successful connection
            Ok((socket, addr)) => {
                info!("Client connected from {}", addr);

                let state = Arc::new(Mutex::new(ConnectionState::Handshake));
                let socket = socket.into_inner()?;
                let socket_read = Async::new(socket.try_clone()?)?;
                let socket_write = Async::new(socket)?;
                let client_proxy = TcpStream::connect("127.0.0.1:25565")?;
                let client_proxy_read = Async::new(client_proxy.try_clone()?)?;
                let client_proxy_write = Async::new(client_proxy)?;

                let state_clone = state.clone();
                EXECUTOR
                    .spawn(async move {
                        handle_connection(
                            state_clone,
                            socket_read,
                            addr,
                            client_proxy_write,
                            ServerBoundPacket::read_from,
                            handle_server_state,
                        )
                        .await
                    })
                    .detach();
                EXECUTOR
                    .spawn(async move {
                        handle_connection(
                            state,
                            client_proxy_read,
                            "server",
                            socket_write,
                            ClientBoundPacket::read_from,
                            handle_client_state,
                        )
                        .await
                    })
                    .detach();
            }

            // Actual error
            Err(e) => error!("Failed to accept TCP socket: {}", e),
        };
    }
}

fn handle_server_state(packet: &ServerBoundPacket, mut state: MutexGuard<'_, ConnectionState>) {
    match packet {
        ServerBoundPacket::Handshake {
            protocol_version,
            next_state,
            ..
        } => {
            if *protocol_version != PROTOCOL_VERSION {
                set_state(&mut *state, ConnectionState::Disconnected);
                return;
            }

            if *next_state == 1 {
                set_state(&mut *state, ConnectionState::Status);
            } else if *next_state == 2 {
                set_state(&mut *state, ConnectionState::Login);
            }
        }
        _ => {}
    }
}

fn handle_client_state(packet: &ClientBoundPacket, mut state: MutexGuard<'_, ConnectionState>) {
    match packet {
        ClientBoundPacket::LoginSuccess { .. } => {
            set_state(&mut *state, ConnectionState::Play);
        }
        _ => {}
    }
}

async fn handle_connection<P: Debug, A: Display>(
    state: Arc<Mutex<ConnectionState>>,
    mut read: Async<TcpStream>,
    read_addr: A,
    mut write: Async<TcpStream>,
    packet_parser: fn(&mut PacketBuffer, ConnectionState, usize) -> Result<P, PacketSerdeError>,
    state_manager: fn(&P, MutexGuard<'_, ConnectionState>),
) -> Result<(), PacketSerdeError> {
    let mut buffer = PacketBuffer::new(4096);

    while *state.lock().await != ConnectionState::Disconnected {
        match read_packet(&mut read, &mut write, &mut buffer, &state).await {
            Ok(packet_len) => {
                // Client disconnected
                if packet_len == 0 {
                    break;
                }
                // Handle the packet
                else {
                    let cursor_start = buffer.cursor();
                    // write.write_all(&buffer[..cursor_start + packet_len]).await.map_err(|e| PacketSerdeError::Network(e))?;
                    let state = state.lock().await;
                    match packet_parser(&mut buffer, *state, packet_len) {
                        Ok(packet) => {
                            state_manager(&packet, state);
                            info!("Received packet from {}\n{:?}", read_addr, packet);
                        }
                        Err(error) => {
                            warn!("Failed to parse packet: {}", error);
                        }
                    }
                    buffer.set_cursor(cursor_start + packet_len);
                }
            }
            Err(e) => {
                error!("Error in connection handler: {}", e);
                shutdown(read)?;
                shutdown(write)?;
                break;
            }
        }
    }

    Ok(())
}

async fn read_packet(
    source: &mut Async<TcpStream>,
    forward_to: &mut Async<TcpStream>,
    buffer: &mut PacketBuffer,
    state: &Arc<Mutex<ConnectionState>>,
) -> Result<usize, PacketSerdeError> {
    if buffer.remaining() > 0 {
        buffer.shift_remaining();

        // Don't decrypt the remaining bytes since that was already handled
        return collect_packet(buffer, source, forward_to).await;
    }
    // Prepare for the next packet
    else {
        buffer.reset_cursor();
    }

    // Inflate the buffer so we can read to its capacity
    buffer.inflate();

    // Read the first chunk, this is what blocks the thread
    let read = source
        .read(&mut buffer[..])
        .await
        .map_err(|error| PacketSerdeError::Network(error))?;
    forward_to
        .write_all(&buffer[.. read])
        .await
        .map_err(|error| PacketSerdeError::Network(error))?;

    // A read of zero bytes means the stream has closed
    if read == 0 {
        set_state(&mut *state.lock().await, ConnectionState::Disconnected);
        buffer.clear();
    }
    // A packet was received
    else {
        // Adjust the buffer length to be that of the bytes read
        buffer.resize(read);

        // The legacy ping packet has no length prefix, so only collect the packet if it's not legacy
        if !(*state.lock().await == ConnectionState::Handshake
            && buffer.peek().unwrap_or(0) as i32 == LEGACY_PING_PACKET_ID)
        {
            return collect_packet(buffer, source, forward_to).await;
        }
    }

    // This is only reached if read == 0 or it's a legacy packet
    Ok(read)
}

async fn collect_packet(
    buffer: &mut PacketBuffer,
    source: &mut Async<TcpStream>,
    forward_to: &mut Async<TcpStream>,
) -> Result<usize, PacketSerdeError> {
    let data_len = buffer.read_varying::<i32>()? as usize;

    // Large packet, gather the rest of the data
    if data_len > buffer.len() {
        let end = buffer.len();
        buffer.resize(buffer.cursor() + data_len);
        source
            .read_exact(&mut buffer[end ..])
            .await
            .map_err(|e| PacketSerdeError::Network(e))?;
        forward_to
            .write_all(&buffer[end ..])
            .await
            .map_err(|e| PacketSerdeError::Network(e))?;
    }

    Ok(data_len)
}

fn shutdown(stream: Async<TcpStream>) -> Result<(), PacketSerdeError> {
    stream
        .into_inner()
        .map_err(|error| PacketSerdeError::Network(error))?
        .shutdown(Shutdown::Both)
        .map_err(|error| PacketSerdeError::Network(error))?;
    Ok(())
}

fn set_state(old_state: &mut ConnectionState, new_state: ConnectionState) {
    info!(
        "Connection state updated from {:?} to {:?}",
        old_state, new_state
    );
    *old_state = new_state;
}
