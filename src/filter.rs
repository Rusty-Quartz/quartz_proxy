use std::collections::HashSet;

pub struct PacketFilter {
    pub filter: HashSet<String>,
    pub suppress_warnings: HashSet<u32>,
    pub is_whitelist: bool,
    pub disabled: bool,
}

impl PacketFilter {
    pub fn new() -> Self {
        PacketFilter {
            filter: HashSet::new(),
            suppress_warnings: HashSet::new(),
            is_whitelist: true,
            disabled: false,
        }
    }

    pub fn update(&mut self, packet: String, deny: bool) -> bool {
        if self.is_whitelist ^ deny {
            self.filter.insert(packet)
        } else {
            self.filter.remove(&packet)
        }
    }

    pub fn update_warning_suppression(&mut self, id: u32, suppress: bool) -> bool {
        if suppress {
            self.suppress_warnings.insert(id)
        } else {
            self.suppress_warnings.remove(&id)
        }
    }

    pub fn reset_as(&mut self, whitelist: bool) {
        self.is_whitelist = whitelist;
        self.filter.clear();
    }

    pub fn set_disabled(&mut self, disabled: bool) -> bool {
        let success = self.disabled != disabled;
        if success {
            self.disabled = disabled;
        }
        success
    }

    pub fn test(&self, packet: &str) -> bool {
        if self.disabled {
            return true;
        }

        let end = packet
            .char_indices()
            .find(|&(_, ch)| ch == ' ')
            .map(|(index, _)| index)
            .unwrap_or(packet.len());
        self.filter.contains(&packet[.. end].to_ascii_lowercase()) == self.is_whitelist
    }

    pub fn should_suppress_warning(&self, id: u32) -> bool {
        if self.disabled {
            return true;
        }

        self.suppress_warnings.contains(&id)
    }
}
