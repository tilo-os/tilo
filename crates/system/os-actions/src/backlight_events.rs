//! The kernel's backlight events. The backlight class announces every level
//! change, from a sysfs write or a firmware hotkey alike, as a `change`
//! uevent, and a backlight that appears or goes as `add` or `remove`; a
//! follower sleeps on the uevent socket until one of those arrives.

/// What happened to a backlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklightEvent {
    /// A backlight's level changed.
    Level,
    /// A backlight appeared or went away.
    Devices,
}

/// The backlight event in one kernel uevent message
/// (`ACTION@DEVPATH\0KEY=VALUE\0...`), if it is one.
pub fn backlight_event(message: &[u8]) -> Option<BacklightEvent> {
    let mut fields = message.split(|&byte| byte == 0);
    let header = fields.next()?;
    let mut action = header.split(|&byte| byte == b'@').next()?;
    let mut backlight = false;
    for field in fields {
        if let Some(value) = field.strip_prefix(b"ACTION=") {
            action = value;
        } else if field == b"SUBSYSTEM=backlight" {
            backlight = true;
        }
    }
    if !backlight {
        return None;
    }
    match action {
        b"change" => Some(BacklightEvent::Level),
        b"add" | b"remove" => Some(BacklightEvent::Devices),
        _ => None,
    }
}

/// Call `event` for every backlight event the kernel sends, on the calling
/// thread, for as long as the uevent socket is open. Returns why it could
/// not be opened or stopped reading.
#[cfg(target_os = "linux")]
pub fn follow(mut event: impl FnMut(BacklightEvent)) -> Result<(), String> {
    use rustix::net::netlink::{self, SocketAddrNetlink};
    use rustix::net::{AddressFamily, RecvFlags, SocketFlags, SocketType, bind, recv, socket_with};
    let socket = socket_with(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        Some(netlink::KOBJECT_UEVENT),
    )
    .map_err(|error| format!("uevent socket: {error}"))?;
    // Group 1 carries the kernel's own messages.
    bind(&socket, &SocketAddrNetlink::new(0, 1))
        .map_err(|error| format!("uevent bind: {error}"))?;
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match recv(&socket, &mut buffer, RecvFlags::empty()) {
            Ok((length, _)) => {
                if let Some(found) = buffer.get(..length).and_then(backlight_event) {
                    event(found);
                }
            }
            Err(rustix::io::Errno::INTR | rustix::io::Errno::NOBUFS) => {}
            Err(error) => return Err(format!("uevent read: {error}")),
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn follow(_event: impl FnMut(BacklightEvent)) -> Result<(), String> {
    Err("backlight events are Linux's".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_backlight_uevents_are_backlight_events() {
        let change = b"change@/devices/platform/backlight/backlight/edp-backlight\0\
ACTION=change\0DEVPATH=/devices/platform/backlight/backlight/edp-backlight\0\
SUBSYSTEM=backlight\0SOURCE=sysfs\0SEQNUM=4242\0";
        assert_eq!(backlight_event(change), Some(BacklightEvent::Level));
        let add = b"add@/devices/x/backlight/acpi_video0\0ACTION=add\0SUBSYSTEM=backlight\0";
        assert_eq!(backlight_event(add), Some(BacklightEvent::Devices));
        let remove = b"remove@/devices/x/backlight/acpi_video0\0SUBSYSTEM=backlight\0";
        assert_eq!(backlight_event(remove), Some(BacklightEvent::Devices));
        let battery =
            b"change@/devices/x/power_supply/BAT0\0ACTION=change\0SUBSYSTEM=power_supply\0";
        assert_eq!(backlight_event(battery), None);
        let bind = b"bind@/devices/x/backlight/y\0ACTION=bind\0SUBSYSTEM=backlight\0";
        assert_eq!(backlight_event(bind), None);
        assert_eq!(backlight_event(b""), None);
    }
}
