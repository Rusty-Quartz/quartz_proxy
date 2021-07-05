use async_executor::Executor;
use async_io::Async;
use async_lock::{Mutex, MutexGuard};
use futures_lite::{future, AsyncReadExt, AsyncWriteExt};
use linefeed::{Interface, ReadResult};
use log::{error, info, warn};
use quartz::{
    chat::{Component, ComponentBuilder, color::PredefinedColor as Color},
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
use once_cell::sync::Lazy;
use std::{u8, collections::HashSet, error::Error, fmt::{Debug, Display}, net::{Shutdown, TcpListener, TcpStream}, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread};

static EXECUTOR: Executor = Executor::new();
static PACKET_FILTER: Lazy<Mutex<PacketFilter>> = Lazy::new(|| Mutex::new(PacketFilter::new()));
static LOG_PACKETS: AtomicBool = AtomicBool::new(true);

struct PacketFilter {
    filter: HashSet<String>,
    is_whitelist: bool,
    disabled: bool
}

impl PacketFilter {
    fn new() -> Self {
        PacketFilter {
            filter: HashSet::new(),
            is_whitelist: true,
            disabled: false
        }
    }

    fn update(&mut self, packet: String, deny: bool) -> bool {
        if self.is_whitelist ^ deny {
            self.filter.insert(packet)
        } else {
            self.filter.remove(&packet)
        }
    }

    fn reset_as(&mut self, whitelist: bool) {
        self.is_whitelist = whitelist;
        self.filter.clear();
    }

    fn set_disabled(&mut self, disabled: bool) -> bool {
        let success = self.disabled != disabled;
        if success {
            self.disabled = disabled;
        }
        success
    }

    fn test(&self, packet: &str) -> bool {
        if self.disabled {
            return true;
        }

        let end = packet.char_indices().find(|&(_, ch)| ch == ' ').map(|(index, _)| index).unwrap_or(packet.len());
        self.filter.contains(&packet[..end].to_ascii_lowercase()) == self.is_whitelist
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Setup the console with a command prompt
    let console_interface = Arc::new(Interface::new("quartz-proxy")?);
    console_interface.set_prompt("> ")?;

    // Initialize logging
    logging::init_logger(Some("quartz_proxy"), console_interface.clone())?;

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
                Ok(result) => match result {
                    ReadResult::Input(command) => {
                        console_interface.add_history_unique(command.clone());

                        match command.as_str() {
                            "stop" => break,
                            cmd => handle_command(cmd).await,
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
                    let state = state.lock().await;
                    let should_log = LOG_PACKETS.load(Ordering::Relaxed);
                    let packet = match packet_parser(&mut buffer, *state, packet_len) {
                        Ok(packet) => packet,
                        Err(error) => {
                            if should_log {
                                warn!("Failed to parse packet: {}, raw buffer:\n{:02X?}", error, &buffer[cursor_start .. cursor_start + packet_len]);
                            }
                            buffer.set_cursor(cursor_start + packet_len);
                            continue;
                        }
                    };
                    state_manager(&packet, state);
                    if should_log {
                        let debug_packet = format!("{:?}", packet);
                        if PACKET_FILTER.lock().await.test(&debug_packet) {
                            info!("Received packet from {}\n{:?}", read_addr, packet);
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

fn display<T: Display>(x: T, color: Color) {
    println!("{}", Component::colored(x.to_string(), color));
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
    if LOG_PACKETS.load(Ordering::Relaxed) {
        info!(
            "Connection state updated from {:?} to {:?}",
            old_state, new_state
        );
    }
    *old_state = new_state;
}

async fn handle_command(command: &str) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut split = trimmed.split(' ');
    let command = split.next();
    if command.is_none() {
        return;
    }
    let mut args = split.collect::<Vec<_>>();

    macro_rules! expect_arg {
        ($($expected:tt)*) => {{
            if args.is_empty() {
                display(format!("Expected additional argument(s): {}", $($expected)*), Color::Red);
                return;
            } else {
                (args.remove(0), || { $($expected)* })
            }
        }}
    }

    match command.unwrap() {
        "pause" => {
            additional_args(&args);

            match LOG_PACKETS.compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => display("Logging paused", Color::Green),
                Err(_) => {
                    println!(
                        "{}",
                        ComponentBuilder::empty()
                            .color(Color::Yellow)
                            .add_text("Logging is already paused. Use command ")
                            .color(Color::Aqua)
                            .add_text("resume")
                            .color(Color::Yellow)
                            .add_text(" to resume logging.")
                            .build()
                    )
                }
            }
        },
        "resume" => {
            additional_args(&args);

            match LOG_PACKETS.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => display("Logging resumed", Color::Green),
                Err(_) => {
                    println!(
                        "{}",
                        ComponentBuilder::empty()
                            .color(Color::Yellow)
                            .add_text("The proxy is currently logging packets. Use command ")
                            .color(Color::Aqua)
                            .add_text("pause")
                            .color(Color::Yellow)
                            .add_text(" to pause logging.")
                            .build()
                    )
                }
            }
        },
        "filter" => {
            const ACTIONS: [&str; 7] = ["allow", "deny", "enable", "disable", "reset", "reset-as", "display"];

            let (action, expected_action) = expect_arg!({
                let mut builder = ComponentBuilder::empty()
                    .color(Color::Red)
                    .add_text("one of ");
                for action in &ACTIONS[..ACTIONS.len() - 1] {
                    builder = builder.color(Color::Gold).add_text(action).add_text(", ");
                }
                builder.add_text("or ").color(Color::Gold).add_text(ACTIONS[ACTIONS.len() - 1]).build()
            });

            let mut filter = PACKET_FILTER.lock().await;

            match action {
                "allow" | "deny" => {
                    let (packet, _) = expect_arg!("packet name");
                    if filter.update(packet.to_ascii_lowercase(), action == "deny") {
                        display("Filter successfully updated", Color::Green);
                    } else {
                        display("This packet is already handled by the filter.", Color::Yellow);
                    }
                },
                "enable" | "disable" => {
                    if filter.set_disabled(action == "disable") {
                        display(format!("Packet filter {}d", action), Color::Green);
                    } else {
                        display(format!("The filter is already {}d", action), Color::Yellow);
                    }
                },
                "reset" => {
                    filter.filter.clear();
                    display("Filter reset.", Color::Green);
                },
                "reset-as" => {
                    let (mode, expected_mode) = expect_arg!({
                        ComponentBuilder::empty()
                            .color(Color::Red)
                            .add_text("one of ")
                            .color(Color::Gold)
                            .add_text("whitelist")
                            .color(Color::Red)
                            .add_text(" or ")
                            .color(Color::Gold)
                            .add_text("blacklist")
                            .build()
                    });

                    match mode {
                        "whitelist" | "blacklist" => {
                            filter.reset_as(mode == "whitelist");
                            display(format!("Reset filter and set mode to {}", mode), Color::Green);
                        },
                        _ => display(format!("Invalid filter mode, expected {}", expected_mode()), Color::Red)
                    }
                },
                "display" => {
                    display(
                        format!(
                            "[{}] {}",
                            if filter.is_whitelist {
                                "whitelist"
                            } else {
                                "blacklist"
                            },
                            if filter.filter.is_empty() {
                                "<empty>".to_owned()
                            } else {
                                let mut elements = filter.filter.iter().cloned().collect::<Vec<_>>();
                                elements.sort();
                                elements.join(", ")
                            }
                        ),
                        Color::White
                    );
                },
                _ => display(format!("Invalid action, expected {}", expected_action()), Color::Red)
            }
        },
        "varint" => {
            if args.is_empty() {
                display("Expected list of space-separated bytes in hexadecimal form", Color::Red);
                return;
            }

            let mut bytes = PacketBuffer::new(args.len());
            for by in args.into_iter().map(|by| if by.ends_with(',') { &by[..by.len() - 1] } else { by }) {
                match u8::from_str_radix(by, 16) {
                    Ok(by) => bytes.write_one(by),
                    Err(e) => display(format!("Failed to parse bytes: {}", e), Color::Red)
                }
            }

            bytes.reset_cursor();
            match bytes.read_varying::<i64>() {
                Ok(value) => display(format!("varint({:02X?}) = {}", &bytes[..bytes.cursor()], value), Color::White),
                Err(error) => display(error, Color::Red)
            }
        }
        _ => display("Invalid command", Color::Red)
    }
}

fn additional_args(args: &[&str]) {
    if !args.is_empty() {
        display(format!("Ignoring additional arguments {}", args.to_owned().join(", ")), Color::Yellow);
    }
}
