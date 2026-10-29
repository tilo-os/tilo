//! The installer on the live medium: the guided setup's destination side.
//!
//! Launched off the live medium under `cage`:
//!     cage -s -- /usr/libexec/tilo/tilo-installer
//! it is the imager app showing the installer's wizard, with the app's
//! standard launch modes (`--shots`, `--headless-drive`, …; its screenshot
//! scenarios are the `destination-*` ones). A live run also answers the
//! `tilo-control` action protocol (actions list, actions invoke, state-dump)
//! on its console endpoints — see crates/session/console-control for where
//! they listen and why only there.
//!
//! `--unattended-file PATH` or `--unattended-fd N` runs an explicit
//! unattended installation from a private recipe through the same published
//! effects. The one-time recovery key goes to `--unattended-recovery-fd N`,
//! `--unattended-recovery-file PATH`, or a real terminal. On any refusal the
//! ordinary wizard keeps answering the console socket so a person can
//! continue interactively.

/// # Errors
///
/// Returns an error for a malformed unattended command line; the live
/// window does not return.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    imager::setup::destination::run()
}
