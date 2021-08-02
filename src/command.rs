use crate::{
    util::{debug_buffer, display},
    LOG_PACKETS,
    LOG_WARNINGS,
    MAX_BUFFER_DISPLAY_LENGTH,
    PACKET_FILTER,
    TIMINGS,
};
use futures_lite::future;
use linefeed::{Completer, Completion, DefaultTerminal, Prompter, Suffix};
use quartz::{
    chat::{color::PredefinedColor as Color, ComponentBuilder},
    network::PacketBuffer,
};
use quartz_commands::{module, CommandModule, Help};
use std::sync::atomic::Ordering;

module! {
    pub mod commands;
    type Context = ();

    command help {
        root executes |_ctx| {
            show_help();
            Ok(())
        };
    }

    command pause {
        root executes |_ctx| {
            match LOG_PACKETS.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst) {
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
            Ok(())
        };
    }

    command resume {
        root executes |_ctx| {
            match LOG_PACKETS.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst) {
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
            Ok(())
        };
    }

    command filter
    where
        "allow" => allow_packet: String,
        "deny" => deny_packet: String,
        "reset-as" => any["whitelist" as reset_as_whitelist, "blacklist" as reset_as_blacklist],
        any["enable", "disable", "reset", "display", help: Help<'cmd>],
    {
        allow_packet executes |_ctx| update_filter(allow_packet, false);
        deny_packet executes |_ctx| update_filter(deny_packet, true);

        reset_as_whitelist executes |_ctx| reset_filter_as("whitelist");
        reset_as_blacklist executes |_ctx| reset_filter_as("blacklist");

        "enable" executes |_ctx| set_filter_enabled("enable");
        "disable" executes |_ctx| set_filter_enabled("disable");

        "reset" executes |_ctx| {
            future::block_on(PACKET_FILTER.lock()).filter.clear();
            display("Filter reset.", Color::Green);
            Ok(())
        };

        "display" executes |_ctx| {
            let filter = future::block_on(PACKET_FILTER.lock());
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
                        let mut elements =
                            filter.filter.iter().cloned().collect::<Vec<_>>();
                        elements.sort();
                        elements.join(", ")
                    }
                ),
                Color::White,
            );
            Ok(())
        };

        help executes |_ctx| {
            show_filter_help();
            Ok(())
        };

        help suggests |_ctx, _arg| Help::suggestions();
    }

    command varint
    where
        bytes: greedy &'cmd str,
    {
        bytes executes |_ctx| {
            let mut buffer = PacketBuffer::new(1);
            for by in bytes.split(' ').map(|by| {
                if by.ends_with(',') {
                    &by[.. by.len() - 1]
                } else {
                    by
                }
            }) {
                match u8::from_str_radix(by, 16) {
                    Ok(by) => buffer.write_one(by),
                    Err(e) => return Err(format!("Failed to parse bytes: {}", e)),
                }
            }

            buffer.reset_cursor();
            match buffer.read_varying::<i64>() {
                Ok(value) => display(
                    format!("varint({:02X?}) = {}", &buffer[.. buffer.cursor()], value),
                    Color::White,
                ),
                Err(error) => return Err(error.to_string()),
            }

            Ok(())
        };
    }

    command string
    where
        bytes: greedy &'cmd str,
    {
        bytes executes |_ctx| {
            let mut buffer = PacketBuffer::new(1);
            for by in bytes.split(' ').map(|by| {
                if by.ends_with(',') {
                    &by[.. by.len() - 1]
                } else {
                    by
                }
            }) {
                match u8::from_str_radix(by, 16) {
                    Ok(by) => buffer.write_one(by),
                    Err(e) => return Err(format!("Failed to parse bytes: {}", e)),
                }
            }

            buffer.reset_cursor();
            match buffer.read::<String>() {
                Ok(string) => display(
                    format!(
                        "string({}) = \"{}\" (length {})",
                        debug_buffer(&buffer[.. buffer.cursor()], 12),
                        string,
                        string.len()
                    ),
                    Color::White,
                ),
                Err(error) => return Err(error.to_string()),
            }

            Ok(())
        };
    }

    command warning | warnings
    where
        "allow" => allow_id: &'cmd str,
        "suppress" => suppress_id: &'cmd str,
        any["on", "off", help: Help<'cmd>],
    {
        allow_id executes |_ctx| update_warnings("allow", allow_id);
        suppress_id executes |_ctx| update_warnings("suppress", suppress_id);

        "on" executes |_ctx| set_warnings_enabled("on");
        "off" executes |_ctx| set_warnings_enabled("off");

        help executes |_ctx| {
            show_warnings_help();
            Ok(())
        };
        help suggests |_ctx, _arg| Help::suggestions();
    }

    command mbdl
    where
        any[len: usize, "none"],
    {
        len executes |_ctx| {
            MAX_BUFFER_DISPLAY_LENGTH.store(len, Ordering::SeqCst);
            display(format!("Buffer display length limit set to {} bytes", len), Color::Green);
            Ok(())
        };

        "none" executes |_ctx| {
            MAX_BUFFER_DISPLAY_LENGTH.store(usize::MAX, Ordering::SeqCst);
            display("Removed buffer display length limit.", Color::Green);
            Ok(())
        };
    }

    command metrics {
        root executes |_ctx| {
            let average = future::block_on(TIMINGS.average());
            display(format!("Processing packets at approximately {:.3} Mb/s", average), Color::White);
            Ok(())
        };
    }
}

// A simple tab-completer for console
pub struct ConsoleCompleter;

impl Completer<DefaultTerminal> for ConsoleCompleter {
    fn complete(
        &self,
        _word: &str,
        prompter: &Prompter<'_, '_, DefaultTerminal>,
        _start: usize,
        _end: usize,
    ) -> Option<Vec<Completion>> {
        Some(
            Commands
                .get_suggestions(&prompter.buffer()[.. prompter.cursor()], &())
                .into_iter()
                .map(|completion| Completion {
                    completion,
                    display: None,
                    suffix: if prompter.cursor() == prompter.buffer().len() {
                        Suffix::Some(' ')
                    } else {
                        Suffix::None
                    },
                })
                .collect(),
        )
    }
}

fn update_filter(packet: String, deny: bool) -> Result<(), String> {
    let mut filter = future::block_on(PACKET_FILTER.lock());
    if filter.update(packet.to_ascii_lowercase(), deny) {
        display("Filter successfully updated", Color::Green);
    } else {
        display(
            "This packet is already handled by the filter.",
            Color::Yellow,
        );
    }

    Ok(())
}

fn reset_filter_as(mode: &str) -> Result<(), String> {
    let mut filter = future::block_on(PACKET_FILTER.lock());
    filter.reset_as(mode == "whitelist");
    display(
        format!("Reset filter and set mode to {}", mode),
        Color::Green,
    );
    Ok(())
}

fn set_filter_enabled(mode: &str) -> Result<(), String> {
    let mut filter = future::block_on(PACKET_FILTER.lock());
    if filter.set_disabled(mode == "disable") {
        display(format!("Packet filter {}d", mode), Color::Green);
    } else {
        display(format!("The filter is already {}d", mode), Color::Yellow);
    }
    Ok(())
}

fn update_warnings(action: &str, id: &str) -> Result<(), String> {
    let id = match u32::from_str_radix(id, 16) {
        Ok(id) => id,
        Err(error) => return Err(format!("Failed to parse ID: {}", error)),
    };

    let mut filter = future::block_on(PACKET_FILTER.lock());

    if filter.update_warning_suppression(id, action == "suppress") {
        display(
            format!("Warnings for packet ID {:02X} {}ed", id, action),
            Color::Green,
        );
    } else {
        display(
            "This packet ID is already handled by the warning filter.",
            Color::Yellow,
        );
    }

    Ok(())
}

fn set_warnings_enabled(mode: &str) -> Result<(), String> {
    match LOG_WARNINGS.compare_exchange(
        mode == "off",
        mode == "on",
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => display(format!("Warnings turned {}", mode), Color::Green),
        Err(_) => display(format!("Warnings are already {}", mode), Color::Yellow),
    }

    Ok(())
}

fn show_help() {
    let mut builder = ComponentBuilder::empty();

    builder = builder
        .color(Color::Gold)
        .add_text("stop: ")
        .color(Color::White)
        .add_text(
            "Stops the proxy and exits the program. It may take a few moments for the program to \
             halt if there are still active connections.\n",
        );

    builder = builder
        .color(Color::Gold)
        .add_text("pause: ")
        .color(Color::White)
        .add_text(
            "Pauses all logging of packets and warnings. Errors and general info messages will \
             still be logged.\n",
        );

    builder = builder
        .color(Color::Gold)
        .add_text("resume: ")
        .color(Color::White)
        .add_text("Resumes all logging of packets and warnings.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("filter <")
        .color(Color::White)
        .add_text("action")
        .color(Color::Gold)
        .add_text("> [")
        .color(Color::White)
        .add_text("params...")
        .color(Color::Gold)
        .add_text("]: ")
        .color(Color::White)
        .add_text("Updates the packet filter based on the given action and parameters. Type ")
        .color(Color::Aqua)
        .add_text("filter -help")
        .color(Color::White)
        .add_text(" for more information.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("varint <")
        .color(Color::White)
        .add_text("bytes...")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text("Parses the given list of bytes as a varint, for example ")
        .color(Color::Aqua)
        .add_text("varint 85 07 00")
        .color(Color::White)
        .add_text(" yields the output ")
        .color(Color::Aqua)
        .add_text("varint([85, 07]) = 901")
        .color(Color::White)
        .add_text(". Trailing commas on the bytes will be ignored.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("string <")
        .color(Color::White)
        .add_text("bytes...")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text("Parses the given list of bytes as a string, for example ")
        .color(Color::Aqua)
        .add_text("string 05 68 65 6C 6C 6F FF FF")
        .color(Color::White)
        .add_text(" yields the output ")
        .color(Color::Aqua)
        .add_text("string([05, 68, 65, 6C, 6C, 6F]) = \"hello\" (length 5)")
        .color(Color::White)
        .add_text(". Trailing commas on the bytes will be ignored.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("warnings <")
        .color(Color::White)
        .add_text("action")
        .color(Color::Gold)
        .add_text("> [")
        .color(Color::White)
        .add_text("params...")
        .color(Color::Gold)
        .add_text("]: ")
        .color(Color::White)
        .add_text("Updates the warnings filter based on the given action and parameters. Type ")
        .color(Color::Aqua)
        .add_text("warnings -help")
        .color(Color::White)
        .add_text(" for more information.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("mbdl <")
        .color(Color::White)
        .add_text("length")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text(
            "Sets the maximum display length when displaying packet buffers to the console. \
             Entering ",
        )
        .color(Color::Aqua)
        .add_text("mbdl none")
        .color(Color::White)
        .add_text(" will remove the display length limit.");

    println!("{}", builder.build());
}

fn show_filter_help() {
    let mut builder = ComponentBuilder::empty();

    builder = builder
        .color(Color::Gold)
        .add_text("filter <")
        .color(Color::White)
        .add_text("allow")
        .color(Color::Gold)
        .add_text("|")
        .color(Color::White)
        .add_text("deny")
        .color(Color::Gold)
        .add_text("> <")
        .color(Color::White)
        .add_text("packet-name")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text(
            "Allows or denies logging of the packet with the given name. This name is case \
             insensitive, but should match the variant name of the packet in its corresponding \
             enum.\n",
        );

    builder = builder
        .color(Color::Gold)
        .add_text("filter <")
        .color(Color::White)
        .add_text("enable")
        .color(Color::Gold)
        .add_text("|")
        .color(Color::White)
        .add_text("disable")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text("Enables or disables the filter.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("filter reset: ")
        .color(Color::White)
        .add_text("Resets the filter, clearing any entries.\n");

    builder = builder
        .color(Color::Gold)
        .add_text("filter reset-as <")
        .color(Color::White)
        .add_text("whitelist")
        .color(Color::Gold)
        .add_text("|")
        .color(Color::White)
        .add_text("blacklist")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text(
            "Resets the filter and changes its mode to the given mode. Whitelist filters allow \
             logging of packets in the filter, whereas blacklist filters only allow logging of \
             packets not in the filter.\n",
        );

    builder = builder
        .color(Color::Gold)
        .add_text("filter display: ")
        .color(Color::White)
        .add_text("Displays the contents and mode of the packet filter.");

    println!("{}", builder.build());
}

fn show_warnings_help() {
    let mut builder = ComponentBuilder::empty();

    builder = builder
        .color(Color::Gold)
        .add_text("warnings <")
        .color(Color::White)
        .add_text("allow")
        .color(Color::Gold)
        .add_text("|")
        .color(Color::White)
        .add_text("suppress")
        .color(Color::Gold)
        .add_text("> <")
        .color(Color::White)
        .add_text("packet-id")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text(
            "Allows or denies warnings associated with the given packet ID. The ID should be in \
             hexadecimal form without any prefix, such as \"2A\" for example.\n",
        );

    builder = builder
        .color(Color::Gold)
        .add_text("warnings <")
        .color(Color::White)
        .add_text("on")
        .color(Color::Gold)
        .add_text("|")
        .color(Color::White)
        .add_text("off")
        .color(Color::Gold)
        .add_text(">: ")
        .color(Color::White)
        .add_text(
            "Enables or disables the logging of warnings associated with packet parsing. Other \
             warnings will still be logged.\n",
        );

    builder = builder
        .color(Color::Gold)
        .add_text("metrics: ")
        .color(Color::White)
        .add_text(
            "Displays the proxy metrics. Currently this just displays the approximate rate of \
             packet processing in megabits per second.",
        );

    println!("{}", builder.build());
}
