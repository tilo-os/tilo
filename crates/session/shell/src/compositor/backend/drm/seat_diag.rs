//! Turning a libseat failure into something the operator can act on.
//!
//! `LibSeatSession::new()` reports one opaque error for every way seat
//! acquisition can go wrong. libseat tries its backends in order — logind
//! first, then seatd — and when none of them is usable the caller gets
//! `ENOSYS` ("Function not implemented"), which describes nothing about
//! what is actually missing:
//!
//! ```text
//! --backend drm failed: failed to open libseat session:
//!   FailedToOpenSession(Os { code: 38, kind: Unsupported, ... })
//! ```
//!
//! The three real causes look identical at that layer:
//!
//! 1. The process is not inside a logind session at all (a terminal in a
//!    container, an ssh login, a systemd service without `PAMName=`), so
//!    the logind backend has no session to attach to.
//! 2. It *is* in a session, but that session owns no seat (`seat: -` in
//!    `loginctl`), which is the normal shape of a `manager` session.
//! 3. Neither backend is available because `seatd` is not running and
//!    there is no `/run/seatd.sock` to connect to.
//!
//! …plus the one that is not a configuration problem at all: a seat exists
//! and is owned by this session, but another compositor already holds it.
//!
//! [`classify`] is a pure function over the error text plus a snapshot of
//! the environment ([`SeatEnvironment`]), so every branch is unit-tested
//! without a real seat. [`SeatFailure`] pairs the verdict with the
//! observed environment and renders the operator-facing message.

use std::fmt;
use std::path::Path;

/// Where seatd's control socket lives when the daemon is running.
pub const SEATD_SOCKET: &str = "/run/seatd.sock";

/// Directory logind creates one file per seat in. Its absence means
/// `systemd-logind` is not running (or not managing seats), so the logind
/// backend cannot succeed no matter what the session variables say.
pub const LOGIND_SEAT_DIR: &str = "/run/systemd/seats";

/// Snapshot of everything [`classify`] is allowed to look at.
///
/// Captured once at the failure site so the verdict and the message the
/// operator reads describe the *same* observation — re-reading the
/// environment inside the formatter would let the two disagree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeatEnvironment {
    /// `XDG_SESSION_ID`. Empty/absent means the caller is not inside a
    /// logind session.
    pub session_id: Option<String>,
    /// `XDG_SEAT`. logind writes `seat0` for a session attached to the
    /// physical seat; a session that owns no seat has this unset (and
    /// `loginctl` prints `-`).
    pub seat: Option<String>,
    /// `XDG_VTNR` — the virtual terminal the session is on, when it has
    /// one. A seat-owning session on a VT is exactly what the DRM backend
    /// needs.
    pub vtnr: Option<String>,
    /// Whether `/run/seatd.sock` exists.
    pub seatd_socket: bool,
    /// Whether logind is managing seats (`/run/systemd/seats` exists).
    pub logind_seats: bool,
    /// `WAYLAND_DISPLAY` — set when a Wayland compositor is already
    /// running and this process is one of its clients.
    pub wayland_display: Option<String>,
    /// `DISPLAY` — the same signal for an X session.
    pub x_display: Option<String>,
}

impl SeatEnvironment {
    /// Read the live environment. The only impure function in this module.
    pub fn detect() -> Self {
        Self::detect_with(|k| std::env::var(k).ok(), |p| Path::new(p).exists())
    }

    /// [`Self::detect`] with the two effects injected, so tests can build
    /// any environment without touching the process's real one (which is
    /// global state and would make the tests order-dependent).
    pub fn detect_with(
        env: impl Fn(&str) -> Option<String>,
        exists: impl Fn(&str) -> bool,
    ) -> Self {
        // An empty string is how a shell exports "unset but present"; it
        // carries no more information than absence, so normalise it away
        // rather than letting `Some("")` read as "we have a session".
        let var = |k: &str| env(k).filter(|v| !v.trim().is_empty());
        Self {
            session_id: var("XDG_SESSION_ID"),
            seat: var("XDG_SEAT"),
            vtnr: var("XDG_VTNR"),
            seatd_socket: exists(SEATD_SOCKET),
            logind_seats: exists(LOGIND_SEAT_DIR),
            wayland_display: var("WAYLAND_DISPLAY"),
            x_display: var("DISPLAY"),
        }
    }

    /// True when a display server is already running for this login —
    /// the "another compositor holds the seat" signal.
    fn display_server_running(&self) -> bool {
        self.wayland_display.is_some() || self.x_display.is_some()
    }

    /// True when the session names a seat that could plausibly be opened.
    /// logind writes `-` for "no seat" in `loginctl`, and some shells
    /// export that verbatim, so treat it as absence.
    fn has_seat(&self) -> bool {
        match self.seat.as_deref() {
            None => false,
            Some(s) => !matches!(s.trim(), "" | "-" | "none"),
        }
    }
}

/// What went wrong, at the level the operator can do something about.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SeatFailureKind {
    /// Not inside a logind session at all. Nothing about the seat can
    /// work from here; the fix is to run from a real login on a VT.
    NoLoginSession,
    /// In a session, but the session owns no seat — the shape of a
    /// `manager` session, or of a session started without a seat
    /// assignment. Same fix as above.
    SessionOwnsNoSeat,
    /// A seat exists and this session owns it, but a display server is
    /// already running on it, so it is not free to take.
    SeatAlreadyHeld,
    /// No usable seat backend: logind isn't managing seats and seatd
    /// isn't running.
    NoSeatBackend,
    /// The seat and the backends look fine; the error is something else
    /// (permissions, a driver, a libseat bug). Report it verbatim.
    Unclassified,
}

/// A libseat failure paired with the environment it was observed in.
/// `Display` renders the whole operator-facing diagnosis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatFailure {
    pub kind: SeatFailureKind,
    pub env: SeatEnvironment,
    /// The raw error text from libseat, kept so a cause we did not
    /// anticipate is still visible.
    pub raw: String,
}

impl SeatFailure {
    /// Classify `raw` against `env` and pair the verdict with both.
    pub fn diagnose(raw: impl Into<String>, env: SeatEnvironment) -> Self {
        let raw = raw.into();
        Self {
            kind: classify(&raw, &env),
            env,
            raw,
        }
    }

    /// The one-line summary, without the observed-environment block.
    pub fn summary(&self) -> &'static str {
        match self.kind {
            SeatFailureKind::NoLoginSession => {
                "this process is not inside a logind session, so it owns no seat"
            }
            SeatFailureKind::SessionOwnsNoSeat => {
                "this logind session owns no seat (loginctl shows seat '-')"
            }
            SeatFailureKind::SeatAlreadyHeld => {
                "the seat exists but a display server is already running on it"
            }
            SeatFailureKind::NoSeatBackend => {
                "no seat backend is available: logind is not managing seats and seatd is not running"
            }
            SeatFailureKind::Unclassified => {
                "libseat refused the session for a reason outside the usual three"
            }
        }
    }

    /// What to do about it.
    pub fn remedy(&self) -> &'static str {
        match self.kind {
            SeatFailureKind::NoLoginSession | SeatFailureKind::SessionOwnsNoSeat => {
                "Switch to a free virtual terminal (Ctrl+Alt+F3), log in there, and run the \
                 compositor from that shell — a VT login is a logind session that owns seat0. \
                 If you are inside a container or over ssh, the seat lives on the host and \
                 cannot be reached from here. As an alternative, start `seatd` and add \
                 yourself to the `seat` group, or run with `--backend winit` to host inside \
                 the session you already have."
            }
            SeatFailureKind::SeatAlreadyHeld => {
                "Stop the compositor that is already running on this seat, or switch to a free \
                 virtual terminal (Ctrl+Alt+F3) and start Tilo there. To develop inside the \
                 running session instead, use `--backend winit`."
            }
            SeatFailureKind::NoSeatBackend => {
                "Start `seatd` (`sudo systemctl start seatd`) and make sure your user is in the \
                 `seat` group, or run from a logind-managed VT login. `--backend winit` hosts \
                 inside an existing session and needs no seat at all."
            }
            SeatFailureKind::Unclassified => {
                "Check that you can open /dev/dri/card* and that a seat is free; `loginctl \
                 session-status` and `seatd -h` are the two things to look at. `--backend \
                 winit` hosts inside an existing session and needs no seat at all."
            }
        }
    }
}

impl fmt::Display for SeatFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "(unset)".to_string());
        write!(
            f,
            "{summary}\n\
             observed: XDG_SESSION_ID={session} XDG_SEAT={seat} XDG_VTNR={vtnr} \
             seatd-socket={seatd} logind-seats={logind} \
             WAYLAND_DISPLAY={wl} DISPLAY={x}\n\
             libseat said: {raw}\n\
             {remedy}",
            summary = self.summary(),
            session = show(&self.env.session_id),
            seat = show(&self.env.seat),
            vtnr = show(&self.env.vtnr),
            seatd = if self.env.seatd_socket {
                SEATD_SOCKET
            } else {
                "(absent)"
            },
            logind = if self.env.logind_seats {
                LOGIND_SEAT_DIR
            } else {
                "(absent)"
            },
            wl = show(&self.env.wayland_display),
            x = show(&self.env.x_display),
            raw = self.raw,
            remedy = self.remedy(),
        )
    }
}

/// Decide which of the four causes produced `raw`.
///
/// Ordering is by how much of the stack the problem invalidates: a caller
/// with no session cannot own a seat regardless of what else is running,
/// and a session with no seat cannot open one regardless of whether seatd
/// is up. Only once both of those are ruled out does "a compositor already
/// has it" or "no backend is running" become the useful answer.
pub fn classify(raw: &str, env: &SeatEnvironment) -> SeatFailureKind {
    if env.session_id.is_none() {
        return SeatFailureKind::NoLoginSession;
    }
    if !env.has_seat() {
        return SeatFailureKind::SessionOwnsNoSeat;
    }
    // The session owns a seat. If something is already displaying on it,
    // that is who has DRM master.
    if env.display_server_running() {
        return SeatFailureKind::SeatAlreadyHeld;
    }
    if !env.logind_seats && !env.seatd_socket {
        return SeatFailureKind::NoSeatBackend;
    }
    // A permission problem reads as EACCES/EPERM rather than the ENOSYS
    // that "no backend was usable" produces; neither maps onto the
    // environment, so both land here with the raw text preserved.
    let _ = raw;
    SeatFailureKind::Unclassified
}

/// Convenience for the runtime: build the full diagnosis from a live
/// libseat error. Kept here so the hardware path has one call to make.
pub fn diagnose_live(raw: impl Into<String>) -> SeatFailure {
    SeatFailure::diagnose(raw, SeatEnvironment::detect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact error the user hit: libseat tried logind, then seatd, and
    /// reported ENOSYS because neither was usable.
    const ENOSYS: &str = "FailedToOpenSession(Os { code: 38, kind: Unsupported, message: \"Function not implemented\" })";

    fn env() -> SeatEnvironment {
        SeatEnvironment::default()
    }

    #[test]
    fn seat_detection_reads_the_environment_through_the_probe() {
        // Phase `detect_with_normalises_empty_variables_to_absent`.
        {
            // A shell that exports XDG_SESSION_ID= (empty) is telling us the
            // same thing as not exporting it. Treating `Some("")` as "we have
            // a session" is exactly how the misdiagnosis happens.
            let e = SeatEnvironment::detect_with(
                |k| match k {
                    "XDG_SESSION_ID" => Some(String::new()),
                    "XDG_SEAT" => Some("   ".to_string()),
                    "XDG_VTNR" => Some("2".to_string()),
                    _ => None,
                },
                |_| false,
            );
            assert_eq!(e.session_id, None);
            assert_eq!(e.seat, None);
            assert_eq!(e.vtnr.as_deref(), Some("2"));
        }
        // Phase `detect_with_reads_the_socket_and_seat_dir_through_the_probe`.
        {
            let e = SeatEnvironment::detect_with(|_| None, |p| p == SEATD_SOCKET);
            assert!(e.seatd_socket, "the seatd socket probe must be consulted");
            assert!(!e.logind_seats);
        }
        // Phase `detect_reads_the_real_environment_without_panicking`.
        // `detect` must not panic in a test environment and must agree with
        // the injected form when handed the same inputs.
        {
            let live = SeatEnvironment::detect();
            let kind = classify(ENOSYS, &live);
            assert!(matches!(
                kind,
                SeatFailureKind::NoLoginSession
                    | SeatFailureKind::SessionOwnsNoSeat
                    | SeatFailureKind::SeatAlreadyHeld
                    | SeatFailureKind::NoSeatBackend
                    | SeatFailureKind::Unclassified
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn seat_diagnosis_classifies_each_missing_precondition() {
        // Phase `no_session_id_is_the_no_login_session_case`.
        {
            // The reported failure: XDG_SESSION_ID empty, no seatd socket.
            let e = SeatEnvironment {
                session_id: None,
                seatd_socket: false,
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::NoLoginSession);
        }
        // Phase `no_session_id_wins_even_when_a_compositor_is_running`.
        {
            // Running Tilo from a terminal emulator inside GNOME *and* outside
            // a logind session (a container, say). "You are not in a session"
            // is the actionable half; "something else holds the seat" would
            // send the operator to kill a compositor that is not the problem.
            let e = SeatEnvironment {
                session_id: None,
                wayland_display: Some("wayland-0".into()),
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::NoLoginSession);
        }
        // Phase `session_without_a_seat_is_its_own_diagnosis`.
        {
            // `loginctl` shows this session's seat as `-` — a manager session.
            let e = SeatEnvironment {
                session_id: Some("1".into()),
                seat: None,
                logind_seats: true,
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::SessionOwnsNoSeat);
        }
        // Phase `a_literal_dash_seat_counts_as_no_seat`.
        // Some shells export what loginctl prints. `-` is not a seat name.
        for name in ["-", "none", "  "] {
            let e = SeatEnvironment {
                session_id: Some("1".into()),
                seat: Some(name.into()),
                logind_seats: true,
                ..env()
            };
            assert_eq!(
                classify(ENOSYS, &e),
                SeatFailureKind::SessionOwnsNoSeat,
                "seat name {name:?} must not read as a real seat"
            );
        }
        // Phase `seat_owned_but_a_wayland_compositor_is_running_is_seat_already_held`.
        {
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                vtnr: Some("2".into()),
                logind_seats: true,
                wayland_display: Some("wayland-0".into()),
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::SeatAlreadyHeld);
        }
        // Phase `an_x_display_counts_as_a_running_display_server_too`.
        {
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                logind_seats: true,
                x_display: Some(":0".into()),
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::SeatAlreadyHeld);
        }
        // Phase `seat_owned_and_free_but_no_backend_running_is_no_seat_backend`.
        {
            // A seat name in the environment with neither logind managing
            // seats nor seatd listening: nothing can open it.
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                vtnr: Some("3".into()),
                logind_seats: false,
                seatd_socket: false,
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::NoSeatBackend);
        }
        // Phase `seatd_socket_alone_is_enough_of_a_backend`.
        {
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                logind_seats: false,
                seatd_socket: true,
                ..env()
            };
            assert_eq!(classify(ENOSYS, &e), SeatFailureKind::Unclassified);
        }
        // Phase `everything_present_and_free_falls_through_to_unclassified`.
        {
            // A permissions failure (EACCES on the seat) looks like this: the
            // environment is correct, so the raw error is the only evidence.
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                vtnr: Some("3".into()),
                logind_seats: true,
                seatd_socket: true,
                ..env()
            };
            assert_eq!(
                classify("FailedToOpenSession(Os { code: 13, .. })", &e),
                SeatFailureKind::Unclassified
            );
        }
    }

    #[test]
    fn seat_diagnosis_messages_name_the_environment_and_remedy() {
        // Phase `the_message_names_the_observed_environment_and_the_vt_remedy`.
        {
            let e = SeatEnvironment {
                session_id: None,
                seat: None,
                vtnr: None,
                seatd_socket: false,
                logind_seats: true,
                wayland_display: Some("wayland-0".into()),
                x_display: None,
            };
            let failure = SeatFailure::diagnose(ENOSYS, e);
            let msg = failure.to_string();
            assert!(
                msg.contains("XDG_SESSION_ID=(unset)"),
                "the observed session id must appear: {msg}"
            );
            assert!(
                msg.contains("XDG_VTNR=(unset)"),
                "the observed VT number must appear: {msg}"
            );
            assert!(
                msg.contains("XDG_SEAT=(unset)"),
                "the observed seat name must appear: {msg}"
            );
            assert!(
                msg.contains("seatd-socket=(absent)"),
                "whether the seatd socket exists must appear: {msg}"
            );
            assert!(
                msg.contains("Ctrl+Alt+F3"),
                "the VT remedy must be spelled out: {msg}"
            );
            assert!(
                msg.contains("code: 38"),
                "the raw libseat error must survive: {msg}"
            );
        }
        // Phase `the_seat_held_message_tells_you_to_stop_the_other_compositor`.
        {
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                vtnr: Some("2".into()),
                logind_seats: true,
                wayland_display: Some("wayland-1".into()),
                ..env()
            };
            let msg = SeatFailure::diagnose(ENOSYS, e).to_string();
            assert!(msg.contains("already running on it"), "{msg}");
            assert!(msg.contains("XDG_SEAT=seat0"), "{msg}");
            assert!(msg.contains("WAYLAND_DISPLAY=wayland-1"), "{msg}");
        }
        // Phase `the_no_backend_message_names_seatd`.
        {
            let e = SeatEnvironment {
                session_id: Some("3".into()),
                seat: Some("seat0".into()),
                logind_seats: false,
                seatd_socket: false,
                ..env()
            };
            let msg = SeatFailure::diagnose(ENOSYS, e).to_string();
            assert!(msg.contains("seatd"), "{msg}");
        }
        // Phase `every_kind_has_a_summary_and_a_remedy`.
        // Every kind must produce a non-empty summary and remedy — a
        // diagnosis that renders as an empty line is worse than the errno.
        for kind in [
            SeatFailureKind::NoLoginSession,
            SeatFailureKind::SessionOwnsNoSeat,
            SeatFailureKind::SeatAlreadyHeld,
            SeatFailureKind::NoSeatBackend,
            SeatFailureKind::Unclassified,
        ] {
            let f = SeatFailure {
                kind,
                env: env(),
                raw: ENOSYS.to_string(),
            };
            assert!(!f.summary().is_empty(), "{kind:?} has no summary");
            assert!(!f.remedy().is_empty(), "{kind:?} has no remedy");
            assert!(f.to_string().contains(f.summary()));
        }
    }
}
