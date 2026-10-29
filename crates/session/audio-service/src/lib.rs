//! The session's audio policy, run as its own process (`tilo-audio`) so the
//! compositor drives no audio graph itself.
//!
//! The compositor starts the helper with the session and reads its standard
//! output: one [`DeviceEvent`] per line, each an audio device arriving or
//! leaving, which the compositor sounds under its own quiet rules (it alone
//! knows whether the screen is being presented). The helper stops when its
//! standard input closes, so it never outlives the compositor that started it.
//!
//! Started with `--serve`, it is the session's audio service instead: it
//! serves the [`sound`] resource over JSON lines ([`tilo_service::lines`])
//! on the same pipes, reading the graph again whenever a device arrives or
//! leaves.

#[cfg(feature = "service")]
pub mod mpris;
#[cfg(feature = "service")]
pub mod policy;
#[cfg(feature = "service")]
pub mod sound;

/// An audio device arrived or left: the one thing the helper tells the
/// compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    Connected,
    Disconnected,
}

impl DeviceEvent {
    /// The line the helper writes for this event.
    #[must_use]
    pub fn line(self) -> &'static str {
        match self {
            Self::Connected => "device-connected",
            Self::Disconnected => "device-disconnected",
        }
    }

    /// The event a line the helper wrote stands for.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "device-connected" => Some(Self::Connected),
            "device-disconnected" => Some(Self::Disconnected),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceEvent;

    #[test]
    fn device_events_read_back_from_their_lines() {
        for event in [DeviceEvent::Connected, DeviceEvent::Disconnected] {
            assert_eq!(DeviceEvent::parse(event.line()), Some(event));
        }
        assert_eq!(DeviceEvent::parse("volume"), None);
    }
}
