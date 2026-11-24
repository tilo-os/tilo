//! This device's network policy that Tilo keeps itself, beside what `NetworkManager` holds per
//! connection: whether an open Wi-Fi network this device keeps joins by itself, or waits for the
//! person.
//!
//! `NetworkManager` autoconnects every saved profile whose own `autoconnect` is set, and it has
//! no setting saying "not an open one". The connectivity service therefore reads this leaf and
//! keeps the saved open profiles in line with it.

pub const SECTION: &str = "network";

/// Whether this device asks before it joins an open Wi-Fi network it keeps: on, no saved network
/// that asks for no password joins by itself; off, each one joins as its own setting says.
pub const ASK_BEFORE_OPEN_NETWORKS: crate::Key<bool> =
    crate::Key::new(SECTION, "ask_before_open_networks", || true);
