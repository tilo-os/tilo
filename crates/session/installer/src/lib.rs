//! qOS graphical installer — library root.
//!
//! A qOS-branded alternative to Anaconda, built on the `ui` toolkit and
//! run under `cage` (a kiosk wlroots compositor) off the live media. It
//! walks the user through a seven-step wizard with a persistent progress
//! breadcrumb:
//!
//!   1. **Keyboard** — pick a layout before typing any secrets.
//!   2. **Network** — optionally connect through `NetworkManager`; offline is valid.
//!   3. **Disk** — pick a target disk (from `lsblk`).
//!   4. **Account** — choose a username + confirmed password.
//!   5. **Options** — filesystem, hostname, time zone.
//!   6. **Review** — read-only summary of every choice + erase warning.
//!   7. **Install** — shell out to the privileged backend on a worker
//!      thread, streaming progress into a live status log, then show a
//!      Finish screen (Reboot / Power off) or a failure screen.
//!
//! The backend is **not** implemented here — the installer only invokes
//! it (see [`install`] for the exact contract).
//!
//! Modules:
//! * [`disks`] — `lsblk` discovery + the output parser.
//! * [`install`] — backend (`/usr/libexec/qos-install`, `/usr/libexec/qos-power`)
//!   invocation + line-streaming.
//! * [`installer_network`] — asynchronous connectivity, Wi-Fi and portal flow.
//! * [`status_log`] — the auto-tailing status view widget.
//! * [`disk_row`] — a selectable disk row widget.
//! * [`step_indicator`] — the numbered step-progress breadcrumb.
//! * [`action_button`] — primary/secondary/disabled call-to-action button.
//! * [`seg_choice`] — CRDT-backed segmented selector (filesystem).
//! * [`layouts`] — curated keyboard-layout dataset (apply + visual data).
//! * [`keyboard_view`] — live visual keyboard reflecting the selection.
//! * [`installer_app`] — the root [`InstallerApp`] `Widget` + wizard
//!   state machine.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod action_button;
pub mod disk_row;
pub mod disks;
pub mod install;
pub mod installer_app;
pub mod installer_network;
pub mod keyboard_view;
pub mod layouts;
pub mod media_profile;
pub mod seg_choice;
pub mod status_log;
pub mod step_indicator;
pub mod timezones;

pub use disks::{Disk, DiskPartition, parse_lsblk, read_disks};
pub use installer_app::{InstallState, InstallerApp, Step};
