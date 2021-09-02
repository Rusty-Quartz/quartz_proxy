use crate::LOG_PACKETS;
use async_io::Async;
use log::info;
use quartz_chat::{color::PredefinedColor as Color, Component};
use quartz_net::{ConnectionState, PacketSerdeError};
use std::{
    fmt::Display,
    net::{Shutdown, TcpStream},
    sync::atomic::Ordering,
};

pub fn display<T: Display>(x: T, color: Color) {
    println!("{}", Component::colored(x.to_string(), color));
}

pub fn shutdown(stream: Async<TcpStream>) -> Result<(), PacketSerdeError> {
    stream
        .into_inner()
        .map_err(|error| PacketSerdeError::Network(error))?
        .shutdown(Shutdown::Both)
        .map_err(|error| PacketSerdeError::Network(error))?;
    Ok(())
}

pub fn set_state(old_state: &mut ConnectionState, new_state: ConnectionState) {
    if LOG_PACKETS.load(Ordering::SeqCst) {
        info!(
            "Connection state updated from {:?} to {:?}",
            old_state, new_state
        );
    }
    *old_state = new_state;
}

pub fn debug_buffer(buffer: &[u8], max_display_length: usize) -> String {
    debug_assert!(max_display_length >= 10, "BUFFER_DISPLAY_THRESHOLD < 10");

    if buffer.len() > max_display_length {
        let count = max_display_length / 3;
        let start = &buffer[.. count];
        let end = &buffer[buffer.len() - count ..];

        format!(
            "[{} ... {}]",
            start
                .iter()
                .map(|by| format!("{:02X}", by))
                .collect::<Vec<_>>()
                .join(", "),
            end.iter()
                .map(|by| format!("{:02X}", by))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!("{:02X?}", buffer)
    }
}
