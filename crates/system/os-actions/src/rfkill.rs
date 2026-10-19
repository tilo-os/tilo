//! The kernel's radio kill switches (`rfkill`), read from sysfs and changed
//! through `/dev/rfkill`.
//!
//! A radio is off when any of its switches blocks it: a *soft* block is a
//! program's (`NetworkManager`, `rfkill block`, a laptop's Fn key handled by
//! its platform driver); a *hard* block is a physical switch no program can
//! undo. `NetworkManager`'s own "Wi-Fi enabled" flag is a third, separate
//! layer, so turning Wi-Fi on has to clear a soft block as well as set that
//! flag, and only the kernel can say whether the radio really is on.

use std::path::Path;

/// The sysfs directory the kernel lists its kill switches in.
const SYS_CLASS_RFKILL: &str = "/sys/class/rfkill";
/// The control device that changes them; logind grants the seat's person
/// access to it.
const DEV_RFKILL: &str = "/dev/rfkill";

/// How the kill switches of one kind of radio stand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RfkillState {
    /// At least one switch of this kind exists.
    pub present: bool,
    /// A program has switched it off.
    pub soft_blocked: bool,
    /// A physical switch has: nothing in software can turn it back on.
    pub hard_blocked: bool,
}

impl RfkillState {
    /// Whether the kill switches let the radio run.
    #[must_use]
    pub fn allows(self) -> bool {
        !self.soft_blocked && !self.hard_blocked
    }
}

/// The kinds of radio the kernel names in `/sys/class/rfkill/*/type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioKind {
    Wlan,
    Bluetooth,
    Wwan,
}

impl RadioKind {
    const fn sysfs_name(self) -> &'static str {
        match self {
            Self::Wlan => "wlan",
            Self::Bluetooth => "bluetooth",
            Self::Wwan => "wwan",
        }
    }

    /// `enum rfkill_type` in `<linux/rfkill.h>`.
    const fn kernel_type(self) -> u8 {
        match self {
            Self::Wlan => 1,
            Self::Bluetooth => 2,
            Self::Wwan => 5,
        }
    }
}

/// Every switch of `kind`, folded together: a machine can have several for
/// one radio (the card's own and the laptop's platform switch), and any one
/// of them blocking keeps the radio off.
#[must_use]
pub fn state(kind: RadioKind) -> RfkillState {
    state_in(Path::new(SYS_CLASS_RFKILL), kind)
}

fn state_in(class: &Path, kind: RadioKind) -> RfkillState {
    let mut state = RfkillState::default();
    let Ok(entries) = std::fs::read_dir(class) else {
        return state;
    };
    for entry in entries.flatten() {
        let root = entry.path();
        if read(&root.join("type")).as_deref() != Some(kind.sysfs_name()) {
            continue;
        }
        state.present = true;
        state.hard_blocked |= read(&root.join("hard")).as_deref() == Some("1");
        // Older kernels only publish `state`: 0 soft-blocked, 1 unblocked,
        // 2 hard-blocked.
        let soft = read(&root.join("soft"));
        state.soft_blocked |= match soft.as_deref() {
            Some(value) => value == "1",
            None => read(&root.join("state")).as_deref() == Some("0"),
        };
    }
    state
}

/// Lift every soft block on `kind`, as `rfkill unblock` does. A hard block
/// stays; [`state`] says whether one remains.
///
/// # Errors
///
/// Returns why `/dev/rfkill` could not be written: it is missing, or this
/// session may not change it.
pub fn unblock(kind: RadioKind) -> Result<(), String> {
    use std::io::Write as _;
    let mut device = std::fs::OpenOptions::new()
        .write(true)
        .open(DEV_RFKILL)
        .map_err(|error| format!("{DEV_RFKILL}: {error}"))?;
    device
        .write_all(&change_all_event(kind, false))
        .map_err(|error| format!("{DEV_RFKILL}: {error}"))
}

/// `struct rfkill_event` (the 8-byte first version every kernel accepts):
/// `idx` (u32, ignored for `CHANGE_ALL`), `type`, `op`, `soft`, `hard`.
fn change_all_event(kind: RadioKind, soft_block: bool) -> [u8; 8] {
    /// `RFKILL_OP_CHANGE_ALL`.
    const OP_CHANGE_ALL: u8 = 3;
    let mut event = [0_u8; 8];
    event[4] = kind.kernel_type();
    event[5] = OP_CHANGE_ALL;
    event[6] = u8::from(soft_block);
    event
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switch(class: &Path, name: &str, kind: &str, soft: &str, hard: &str) {
        let root = class.join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("type"), format!("{kind}\n")).unwrap();
        std::fs::write(root.join("soft"), format!("{soft}\n")).unwrap();
        std::fs::write(root.join("hard"), format!("{hard}\n")).unwrap();
    }

    #[test]
    fn any_switch_of_a_kind_blocking_keeps_that_radio_off() {
        let class = tempfile::tempdir().unwrap();
        assert_eq!(
            state_in(class.path(), RadioKind::Wlan),
            RfkillState::default(),
            "no switch: nothing to say"
        );
        switch(class.path(), "rfkill0", "wlan", "0", "0");
        switch(class.path(), "rfkill1", "bluetooth", "1", "0");
        let wlan = state_in(class.path(), RadioKind::Wlan);
        assert!(wlan.present && wlan.allows());
        // The laptop's platform switch blocks Wi-Fi too.
        switch(class.path(), "rfkill2", "wlan", "1", "0");
        let wlan = state_in(class.path(), RadioKind::Wlan);
        assert!(wlan.soft_blocked && !wlan.hard_blocked && !wlan.allows());
        switch(class.path(), "rfkill3", "wlan", "0", "1");
        assert!(state_in(class.path(), RadioKind::Wlan).hard_blocked);
        assert!(state_in(class.path(), RadioKind::Bluetooth).soft_blocked);
        assert!(!state_in(class.path(), RadioKind::Wwan).present);
    }

    #[test]
    fn the_unblock_event_is_the_kernels_change_all_for_that_radio() {
        assert_eq!(
            change_all_event(RadioKind::Wlan, false),
            [0, 0, 0, 0, 1, 3, 0, 0]
        );
        assert_eq!(
            change_all_event(RadioKind::Wwan, true),
            [0, 0, 0, 0, 5, 3, 1, 0]
        );
    }
}
