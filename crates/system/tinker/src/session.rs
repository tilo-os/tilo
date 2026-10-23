//! Constructing the sandbox a layer session runs in.
//!
//! The argument vector is built as data and rendered separately from being
//! executed, so the exact confinement of a session is a testable value rather
//! than something observable only by running it. `tinker shell --explain`
//! prints the same vector used for the session.
//!
//! Defaults, all of which are deliberate:
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

/// A host path exposed inside a layer session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub host: PathBuf,
    /// Where it appears inside the layer. Defaults to the same path.
    pub guest: PathBuf,
    pub writable: bool,
}

/// Everything that decides what a session can do.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Base root filesystem, the overlay's bottom-most lower directory.
    pub base: PathBuf,
    /// Lower directories stacked above the base, base-most first.
    pub lower_layers: Vec<PathBuf>,
    /// Where this session's writes go, and its diff is read from.
    pub upper: PathBuf,
    pub work: PathBuf,
    /// Becomes `/root` inside.
    pub home: PathBuf,
    /// Shown as the hostname inside, so the prompt can say where you are.
    pub layer_name: String,
    pub shares: Vec<Share>,
    /// `false` unshares the network namespace.
    pub network: bool,
    /// Command to run; empty means an interactive login shell.
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

    /// The full `bwrap` argument vector, excluding `bwrap` itself.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SessionSpec {
        SessionSpec::new(
            "/s/bases/debian",
            "/s/layers/a/upper",
            "/s/layers/a/work",
            "/s/layers/a/home",
            "a",
        )
    }

    fn strs(spec: &SessionSpec) -> Vec<String> {
        spec.to_bwrap_args()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    /// Index of the first occurrence of `needle`.
    fn pos(v: &[String], needle: &str) -> usize {
        v.iter()
            .position(|s| s == needle)
            .unwrap_or_else(|| panic!("missing {needle} in {v:?}"))
    }

    #[test]
    fn sessions_isolate_the_host_home_network_and_namespaces() {
        {
            let v = strs(&spec());
            // The failure mode being guarded is a `--bind / /` or `--dev-bind / /`
            // creeping in for convenience, which would make the whole layer
            // pointless.
            for i in 0..v.len().saturating_sub(2) {
                let is_bind = matches!(v[i].as_str(), "--bind" | "--dev-bind" | "--ro-bind");
                assert!(
                    !(is_bind && v[i + 1] == "/"),
                    "session binds the host root: {:?}",
                    &v[i..i + 3]
                );
            }
        }
        {
            let v = strs(&spec());
            let i = pos(&v, "/s/layers/a/home");
            assert_eq!(v[i - 1], "--bind");
            assert_eq!(v[i + 1], "/root");
            let h = pos(&v, "HOME");
            assert_eq!(v[h + 1], "/root");
            assert!(!v.iter().any(|s| s.starts_with("/home/")), "{v:?}");
        }
        {
            let v = strs(&spec());
            let i = pos(&v, "--chmod");
            assert_eq!(v[i + 1], "1777");
            assert_eq!(v[i + 2], "/tmp");
        }
        {
            // The id mapping is established before bwrap; asking bwrap for another
            // user namespace would collapse the map back to a single uid.
            let v = strs(&spec());
            assert!(
                !v.iter()
                    .any(|s| s == "--unshare-user" || s == "--unshare-all"),
                "{v:?}"
            );
            assert!(v.iter().any(|s| s == "--unshare-pid"));
            assert!(v.iter().any(|s| s == "--die-with-parent"));
        }
        {
            let mut s = spec();
            s.network = false;
            let v = strs(&s);
            assert!(v.iter().any(|x| x == "--unshare-net"));
            assert!(!v.iter().any(|x| x == "/etc/resolv.conf"));
        }
        {
            let mut s = spec();
            s.shares = vec![
                Share {
                    host: "/data/in".into(),
                    guest: "/mnt/in".into(),
                    writable: false,
                },
                Share {
                    host: "/data/out".into(),
                    guest: "/mnt/out".into(),
                    writable: true,
                },
            ];
            let v = strs(&s);
            let i = pos(&v, "/data/in");
            assert_eq!(v[i - 1], "--ro-bind");
            assert_eq!(v[i + 1], "/mnt/in");
            let j = pos(&v, "/data/out");
            assert_eq!(v[j - 1], "--bind");
            assert_eq!(v[j + 1], "/mnt/out");
        }
    }

    #[test]
    fn session_argv_stacks_overlays_and_places_the_command() {
        {
            let mut s = spec();
            s.lower_layers = vec![
                PathBuf::from("/s/layers/bottom/upper"),
                PathBuf::from("/s/layers/mid/upper"),
            ];
            let v = strs(&s);
            let base = pos(&v, "/s/bases/debian");
            let bottom = pos(&v, "/s/layers/bottom/upper");
            let mid = pos(&v, "/s/layers/mid/upper");
            let upper = pos(&v, "/s/layers/a/upper");
            // bwrap gives later --overlay-src higher precedence, so this ordering is
            // what makes a stacked layer actually shadow the one below it.
            assert!(base < bottom && bottom < mid && mid < upper, "{v:?}");
            assert_eq!(v[upper - 1], "--overlay");
            assert_eq!(v[upper + 1], "/s/layers/a/work");
            assert_eq!(v[upper + 2], "/");
        }
        {
            let v = strs(&spec());
            let sep = pos(&v, "--");
            assert_eq!(&v[sep + 1..], &["/bin/sh".to_string(), "-l".to_string()]);
        }
        {
            let mut s = spec();
            s.command = vec![
                "apt-get".into(),
                "install".into(),
                "-y".into(),
                "cowsay".into(),
            ];
            let v = strs(&s);
            let sep = pos(&v, "--");
            assert_eq!(&v[sep + 1..], &["apt-get", "install", "-y", "cowsay"]);
        }
        {
            let text = spec().explain();
            assert!(text.starts_with("bwrap "), "{text}");
            assert!(
                text.contains("--overlay /s/layers/a/upper /s/layers/a/work /"),
                "{text}"
            );
        }
        {
            let v = strs(&spec());
            let i = pos(&v, "TILO_LAYER");
            assert_eq!(v[i + 1], "a");
            let h = pos(&v, "--hostname");
            assert_eq!(v[h + 1], "a");
        }
    }
}
