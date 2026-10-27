//! Tilo graphical installer — library root.
//!
//! A Tilo-branded alternative to Anaconda, built on the `ui` toolkit and
//! run under `cage` (a kiosk wlroots compositor) off the live media. It
//! walks the user through an eight-step wizard with a persistent progress
//! breadcrumb:
//!
//!   1. **Keyboard** — pick a layout before typing any secrets.
//!   2. **Network** — optionally connect through `NetworkManager`; offline is valid.
//!   3. **Disk** — pick a target disk (from `lsblk`).
//!   4. **Account** — choose a username + confirmed password.
//!   5. **Options** — filesystem, hostname, time zone.
//!   6. **Media** — explicitly choose whether to download more formats.
//!   7. **Review** — read-only summary of every choice + erase warning.
//!   8. **Install** — shell out to the privileged backend on a worker
//!      thread, streaming progress into a live status log, then show a
//!      Finish screen (Reboot / Power off) or a failure screen.
//!
//! The backend is **not** implemented here — the installer only invokes
//! it (see [`install`] for the exact contract).
//!
//! Every step is also a set of published actions (resources
//! `installer`, `installer.keyboard`, `installer.network`,
//! `installer.storage` with one `installer.disk:<dev>` per disk,
//! `installer.account`, `installer.options`, `installer.media`, `installer.review`,
//! `installer.recovery`, `installer.power`), and the wizard's own controls
//! run those actions — there is one implementation behind a click, a screen
//! reader, the Ctrl+K palette, a headless scenario and the console. The
//! destructive `begin` carries a typed-phrase confirmation contract naming
//! the exact object it formats. On the live medium the console answers the
//! `tilo-control` action protocol (see `console-control`); nothing about it
//! can reveal a passphrase or the recovery key.
//!
//! Modules:
//! * [`disks`] — `lsblk` discovery + the output parser.
//! * [`install`] — backend (`/usr/libexec/tilo-install`, `/usr/libexec/tilo-power`)
//!   invocation + line-streaming.
//! * [`installer_network`] — asynchronous connectivity, Wi-Fi and portal flow.
//! * [`internal_handoff`] — the staged (USB-free) installation handoff.
//! * [`status_log`] — the auto-tailing status view widget.
//! * [`disk_row`] — a selectable disk row widget.
//! * [`step_indicator`] — the numbered step-progress breadcrumb.
//! * [`action_button`] — primary/secondary/disabled call-to-action button.
//! * [`control`] — what a control does when used: queue its published action.
//! * [`seg_choice`] — segmented selector over a published action (filesystem).
//! * [`layouts`] — curated keyboard-layout dataset (apply + visual data).
//! * [`keyboard_view`] — live visual keyboard reflecting the selection.
//! * [`installer_app`] — the root [`InstallerApp`] `Widget` + wizard
//!   state machine, its published surface and its console.
//! * [`operation`] — the one payload type every door produces.
//! * [`backend`] — the side-effecting seam (disks, install, power) that
//!   host tests replace with a recording fake.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod account_handoff;
pub mod action_button;
pub mod backend;
pub mod control;
pub mod disk_row;
pub mod disks;
mod i18n;
pub mod install;
pub mod installer_app;
pub mod installer_network;
pub mod internal_handoff;
pub mod keyboard_view;
pub mod layouts;
pub mod media_profile;
pub mod online_accounts;
pub mod operation;
pub mod power_readiness;
pub mod seg_choice;
pub mod status_log;
pub mod step_indicator;
pub mod timezones;
pub mod unattended;
pub mod windows_hint_source;

pub use disks::{Disk, DiskPartition, parse_lsblk, read_disks};
pub use installer_app::{InstallState, InstallerApp, Step};

/// Layout of a full-width wizard row that keeps its `height` instead of
/// shrinking when the column runs short.
fn full_width_row(height: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::length(height),
        },
        flex_shrink: 0.0,
        ..Default::default()
    }
}
