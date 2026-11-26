//! Constructing the sandbox a layer session runs in.
//!
//! The argument vector is built as data and rendered separately from being
//! executed, so the exact confinement of a session is a testable value rather
//! than something observable only by running it. `tinker shell --explain`
//! prints the same vector used for the session.
//!
//! * uid 0 inside, mapped to the invoking user outside;
//! * the host root filesystem is not visible at all — the layer's root is the
//!   base image plus its own overlay;
//! * `$HOME` is the layer's own home, not the user's. Sharing the host home
//!   directory into a sandbox, as the common container wrappers do, hands
//!   anything running inside it every file, key and shell profile the user owns
//!   and makes the sandbox decorative;
//! * host paths appear only when named one at a time, and read-only unless asked
//!   for otherwise;
//! * the network is on, because package managers need network access.

use std::ffi::OsString;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub host: PathBuf,
    pub guest: PathBuf,
    pub writable: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub base: PathBuf,
    pub lower_layers: Vec<PathBuf>,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub home: PathBuf,
    pub layer_name: String,
    pub shares: Vec<Share>,
    pub network: bool,
    pub command: Vec<OsString>,
}

impl SessionSpec {
    pub fn new(
        base: impl Into<PathBuf>,
        upper: impl Into<PathBuf>,
        work: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
        layer_name: impl Into<String>,
    ) -> Self {
        SessionSpec {
            base: base.into(),
            lower_layers: Vec::new(),
            upper: upper.into(),
            work: work.into(),
            home: home.into(),
            layer_name: layer_name.into(),
            shares: Vec::new(),
            network: true,
            command: Vec::new(),
        }
    }

    pub fn to_bwrap_args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = Vec::new();
        macro_rules! push {
            ($($x:expr),+ $(,)?) => { $( a.push(OsString::from($x)); )+ };
        }

        // The user namespace is entered before bwrap runs, by `unshare(1)` with
        // the user's subordinate id range, so that service accounts inside the
        // layer are real uids. bwrap therefore must not create a second one.
        push!("--unshare-pid");
        push!("--unshare-uts");
        push!("--unshare-ipc");
        push!("--unshare-cgroup-try");
        push!("--die-with-parent");
        push!("--new-session");

        if !self.network {
            push!("--unshare-net");
        }

        // Overlay: base first, stacked layers above it, topmost last. bwrap
        // gives later --overlay-src arguments higher precedence.
        a.push(OsString::from("--overlay-src"));
        a.push(self.base.clone().into());
        for lower in &self.lower_layers {
            a.push(OsString::from("--overlay-src"));
            a.push(lower.clone().into());
        }
        a.push(OsString::from("--overlay"));
        a.push(self.upper.clone().into());
        a.push(self.work.clone().into());
        a.push(OsString::from("/"));

        push!("--proc");
        push!("/proc");
        push!("--dev");
        push!("/dev");
        push!("--tmpfs");
        push!("/run");
        push!("--tmpfs");
        push!("/tmp");
        // A 0755 /tmp breaks any package manager that drops privilege to a
        // service account before writing there — apt fails to mkstemp and the
        // whole install aborts.
        push!("--chmod");
        push!("1777");
        push!("/tmp");

        a.push(OsString::from("--bind"));
        a.push(self.home.clone().into());
        a.push(OsString::from("/root"));

        if self.network {
            // Name resolution is configuration, not connectivity: without this a
            // shared network namespace still cannot resolve anything.
            for f in ["/etc/resolv.conf"] {
                if Path::new(f).exists() {
                    a.push(OsString::from("--ro-bind-try"));
                    a.push(OsString::from(f));
                    a.push(OsString::from(f));
                }
            }
        }

        for share in &self.shares {
            a.push(OsString::from(if share.writable {
                "--bind"
            } else {
                "--ro-bind"
            }));
            a.push(share.host.clone().into());
            a.push(share.guest.clone().into());
        }

        push!("--hostname");
        a.push(OsString::from(&self.layer_name));
        push!("--setenv");
        push!("HOME");
        push!("/root");
        push!("--setenv");
        push!("PATH");
        push!("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
        push!("--setenv");
        push!("DEBIAN_FRONTEND");
        push!("noninteractive");
        // The host's locale is meaningless inside a foreign base image and makes
        // every maintainer script complain about a locale it cannot find.
        push!("--setenv");
        push!("LANG");
        push!("C.UTF-8");
        // So a shell prompt, a build script, or anything else can tell it is in a
        // layer and which one. Being able to see where you are is the difference
        // between confinement and disorientation.
        push!("--setenv");
        push!("TILO_LAYER");
        a.push(OsString::from(&self.layer_name));
        push!("--chdir");
        push!("/root");

        push!("--");
        if self.command.is_empty() {
            push!("/bin/sh");
            push!("-l");
        } else {
            a.extend(self.command.iter().cloned());
        }
        a
    }

    /// The command line as a person would type it, for `--explain`.
    pub fn explain(&self) -> String {
        let mut out = String::from("bwrap");
        for arg in self.to_bwrap_args() {
            let s = arg.to_string_lossy();
            out.push(' ');
            if s.starts_with("--") {
                out.push_str(&s);
            } else if s.contains(char::is_whitespace) {
                let _ = write!(out, "'{s}'");
            } else {
                out.push_str(&s);
            }
        }
        out
    }
}
