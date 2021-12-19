mod command;
mod filter;
mod timings;
mod util;

use async_executor::Executor;
use async_io::Async;
use async_lock::{Mutex, MutexGuard};
use futures_lite::{future, AsyncReadExt, AsyncWriteExt};
use linefeed::{Interface, ReadResult};
use log::{error, info, warn};
use once_cell::sync::Lazy;
use quartz_chat::color::PredefinedColor as Color;
use quartz_commands::CommandModule;
use quartz_net::{
    ClientBoundPacket,
    ConnectionState,
    PacketBuffer,
    PacketSerdeError,
    ServerBoundPacket,
    LEGACY_PING_PACKET_ID,
    PROTOCOL_VERSION,
};
use quartz_util::logging;
use std::{
    error::Error,
    fmt::{Debug, Display},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

use command::*;
use filter::*;
use timings::*;
use util::*;

pub static EXECUTOR: Executor = Executor::new();
pub static PACKET_FILTER: Lazy<Mutex<PacketFilter>> = Lazy::new(|| Mutex::new(PacketFilter::new()));
pub static LOG_PACKETS: AtomicBool = AtomicBool::new(true);
pub static LOG_WARNINGS: AtomicBool = AtomicBool::new(true);
pub static MAX_BUFFER_DISPLAY_LENGTH: AtomicUsize = AtomicUsize::new(64);
pub static TIMINGS: Timings = Timings::new(50.0);

fn main() -> Result<(), Box<dyn Error>> {
    // Setup the console with a command prompt
    let console_interface = Arc::new(Interface::new("quartz-proxy")?);
    console_interface.set_prompt("> ")?;
    console_interface.set_completer(Arc::new(ConsoleCompleter));

    // Initialize logging
    logging::init_logger(
        Some(|path| path.starts_with("quartz")),
        console_interface.clone(),
    )?;

    let proxy_server = match TcpListener::bind("0.0.0.0:25566") {
        Ok(listener) => listener,
        Err(e) => {
            // TODO: make server ip/port configurable
            error!("Failed to bind to address: {}", e);
            return Ok(());
        }
    };
    let proxy_server = match Async::new(proxy_server) {
        Ok(listener) => listener,
        Err(e) => {
            error!("Failed to convert TCP listener to async I/O: {}", e);
            return Ok(());
        }
    };

    let (shutdown_tx, shutdown_rx) = async_channel::bounded::<()>(1);

    for i in 1 ..= 4 {
        let shutdown_rx_clone = shutdown_rx.clone();

        if let Err(e) = thread::Builder::new()
            .name(format!("quartz_proxy#{}", i))
            .spawn(move || future::block_on(EXECUTOR.run(shutdown_rx_clone.recv())))
        {
            error!("{}", e);
            let _ = future::block_on(shutdown_tx.send(()));
            return Ok(());
        }
    }

    EXECUTOR
        .spawn(async move {
            if let Err(e) = handle_incoming_connection(proxy_server).await {
                error!("{}", e);
            }
        })
        .detach();

    future::block_on(async move {
        loop {
            match console_interface.read_line() {
                Ok(result) =>
                    if let ReadResult::Input(command) = result {
                        console_interface.add_history_unique(command.clone());

                        match command.as_str() {
                            "stop" => break,
                            cmd =>
                                if let Err(e) = Commands.dispatch(cmd.trim(), ()) {
                                    display(e, Color::Red);
                                },
                        }
                    },
                Err(e) => {
                    error!("Failed to read console input: {}", e);
                    break;
                }
            }
        }
    });

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
    if let ServerBoundPacket::Handshake {
        protocol_version,
        next_state,
        ..
    } = packet
    {
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
) {
    let mut buffer = PacketBuffer::new(4096);

    while *state.lock().await != ConnectionState::Disconnected {
        match read_packet(&mut read, &mut write, &mut buffer, &state).await {
            Ok((packet_len, timer)) => {
                // Client disconnected
                if packet_len == 0 {
                    break;
                }
                // Handle the packet
                else {
                    handle_packet(
                        &mut buffer,
                        &state,
                        &read_addr,
                        packet_len,
                        timer,
                        packet_parser,
                        state_manager,
                    )
                    .await;
                }
            }
            Err(e) => {
                error!("Error in connection handler: {}", e);
                let _ = shutdown(read);
                let _ = shutdown(write);
                break;
            }
        }
    }
}

async fn handle_packet<P: Debug, A: Display>(
    buffer: &mut PacketBuffer,
    state: &Arc<Mutex<ConnectionState>>,
    read_addr: &A,
    packet_len: usize,
    timer: Timer<'_>,
    packet_parser: fn(&mut PacketBuffer, ConnectionState, usize) -> Result<P, PacketSerdeError>,
    state_manager: fn(&P, MutexGuard<'_, ConnectionState>),
) {
    let cursor_start = buffer.cursor();
    let state = state.lock().await;

    let result = packet_parser(buffer, *state, packet_len);
    timer.finish(cursor_start + packet_len).await;

    match result {
        Ok(packet) => {
            state_manager(&packet, state);
            if LOG_PACKETS.load(Ordering::SeqCst) {
                let debug_packet = format!("{:?}", packet);
                if PACKET_FILTER.lock().await.test(&debug_packet) {
                    info!("Received packet from {}\n{:?}", read_addr, packet);
                }
            }
        }
        Err(error) =>
            if LOG_PACKETS.load(Ordering::SeqCst) && LOG_WARNINGS.load(Ordering::SeqCst) {
                buffer.set_cursor(cursor_start);
                let suppress = match buffer.read_varying::<i32>() {
                    Ok(id) => PACKET_FILTER
                        .lock()
                        .await
                        .should_suppress_warning(id as u32),
                    Err(e) => {
                        warn!("Failed to detect packet ID: {}", e);
                        false
                    }
                };

                if *state != ConnectionState::Disconnected && !suppress {
                    warn!(
                        "Failed to parse packet from {}: {}, raw buffer:\n{}",
                        read_addr,
                        error,
                        debug_buffer(
                            &buffer[cursor_start .. cursor_start + packet_len],
                            MAX_BUFFER_DISPLAY_LENGTH.load(Ordering::SeqCst)
                        )
                    );
                }
            },
    };

    // In the event that we fail horribly at parsing the packet, this should keep us on-track
    buffer.set_cursor(cursor_start + packet_len);
}

pub async fn read_packet(
    source: &mut Async<TcpStream>,
    forward_to: &mut Async<TcpStream>,
    buffer: &mut PacketBuffer,
    state: &Arc<Mutex<ConnectionState>>,
) -> Result<(usize, Timer<'static>), PacketSerdeError> {
    let timer = TIMINGS.spawn_timer();

    if buffer.remaining() > 0 {
        buffer.shift_remaining();

        // Don't decrypt the remaining bytes since that was already handled
        return Ok((collect_packet(buffer, source, forward_to).await?, timer));
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
        .map_err(PacketSerdeError::Network)?;
    forward_to
        .write_all(&buffer[.. read])
        .await
        .map_err(PacketSerdeError::Network)?;

    // Reset the timer since we don't know how long we'll be waiting for data
    let timer = TIMINGS.spawn_timer();

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
            && buffer.peek_one().unwrap_or(0) as i32 == LEGACY_PING_PACKET_ID)
        {
            return Ok((collect_packet(buffer, source, forward_to).await?, timer));
        }
    }

    // This is only reached if read == 0 or it's a legacy packet
    Ok((read, timer))
}

pub async fn collect_packet(
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
            .map_err(PacketSerdeError::Network)?;
        forward_to
            .write_all(&buffer[end ..])
            .await
            .map_err(PacketSerdeError::Network)?;
    }

    Ok(data_len)
}
