//! A [`SeatbeltPolicy`] rendered as an SBPL profile.
//!
//! Deny by default. The profile text depends only on the policy's *shape*
//! (how many grants, which network mode); every path reaches it as a
//! parameter (`(param "PAYLOAD_DIR")`, passed with `sandbox-exec -D`), so no
//! path is ever spliced into SBPL and none needs escaping.
//!
//! Baseline: Apple's `system.sb` — the dyld shared cache, `/System`,
//! `/usr/lib`, `/usr/share`, `/dev/null`/`urandom`, logging and
//! `cfprefsd`, i.e. what every process needs and no user content — plus its
//! `(system-graphics)` macro for Metal (`IOAccelerator` and `IOSurface` user
//! clients, `CoreAnimation`, `CoreDisplay`). On top of that, a fixed set of
//! Mach services a winit/`AppKit` window needs to reach `WindowServer`, fonts,
//! pasteboard and input methods; the same two rules as the Linux read
//! profile (`launcher::profile`) govern it: no user content, and every GUI
//! app needs it.
//!
//! Write-xor-execute holds at the path level: code may be mapped and exec'd
//! from the payload and the runtime directory only, and writes there are
//! denied last so that no user grant can re-open them (in SBPL the last
//! matching rule wins).
//!
//! Tilo's private stores are denied the same way, after every grant: no
//! access at all to the store, and no write to any directory or symlink on
//! its way ([`SeatbeltPolicy::hidden`], [`SeatbeltPolicy::pinned`]).

use std::fmt::Write as _;
use std::path::Path;

use crate::policy::{SeatbeltNetwork, SeatbeltPolicy};

pub const PAYLOAD_PARAM: &str = "PAYLOAD_DIR";
pub const RUNTIME_PARAM: &str = "RUNTIME_DIR";
pub const CONTAINER_PARAM: &str = "CONTAINER_DIR";
pub const DARWIN_CACHE_PARAM: &str = "DARWIN_CACHE_DIR";
pub const GATE_PORT_PARAM: &str = "NET_GATE_PORT";
pub const READ_PARAM_PREFIX: &str = "READ_";
/// The sandbox-extension class the runtime's chooser issues for a picked
/// path (App Sandbox's user-selected read-write).
pub const USER_SELECTED_EXTENSION: &str = "com.apple.app-sandbox.read-write";
pub const WRITE_PARAM_PREFIX: &str = "WRITE_";
/// A private store: denied outright ([`SeatbeltPolicy::hidden`]).
pub const HIDDEN_PARAM_PREFIX: &str = "HIDDEN_";
/// A path a private store resolves through: never written
/// ([`SeatbeltPolicy::pinned`]).
pub const PINNED_PARAM_PREFIX: &str = "PINNED_";

/// Mach services an `AppKit` window needs beyond `system.sb`: `WindowServer`
/// and the window manager, Dock (activation, fullscreen), `LaunchServices`,
/// pasteboard, text input and input methods, HID events, fonts, icons and
/// distributed notifications (appearance and locale changes).
pub const GUI_MACH_SERVICES: &[&str] = &[
    "com.apple.windowserver.active",
    "com.apple.windowmanager.server",
    "com.apple.window_proxies",
    "com.apple.dock.server",
    "com.apple.dock.fullscreen",
    "com.apple.CoreServices.coreservicesd",
    "com.apple.coreservices.launchservicesd",
    "com.apple.lsd.mapdb",
    "com.apple.pasteboard.1",
    "com.apple.tsm.uiserver",
    "com.apple.inputmethodkit.getxpcendpoint",
    "com.apple.inputmethodkit.launchagent",
    "com.apple.inputmethodkit.launcher",
    "com.apple.inputmethodkit.setxpcendpoint",
    "com.apple.iohideventsystem",
    "com.apple.backboard.hid-services.xpc",
    "com.apple.FontObjectsServer",
    "com.apple.fonts",
    "com.apple.iconservices",
    "com.apple.iconservices.store",
    "com.apple.distributed_notifications@Uv3",
];

/// Preference domains `AppKit` and `CoreText` read at startup (appearance,
/// keyboard, accessibility, font rendering). Read-only.
pub const GUI_PREFERENCE_DOMAINS: &[&str] = &[
    "kCFPreferencesAnyApplication",
    "com.apple.AppKit",
    "com.apple.HIToolbox",
    "com.apple.Accessibility",
    "com.apple.universalaccess",
    "com.apple.ATS",
    "com.apple.CoreGraphics",
    "com.apple.inputmethodkit",
];

/// System files outside `system.sb` every GUI app reads: installed system
/// fonts and `AppKit`'s view-bridge configuration.
pub const GUI_READ_SUBPATHS: &[&str] = &["/Library/Fonts"];
pub const GUI_READ_LITERALS: &[&str] = &["/Library/Preferences/com.apple.ViewBridge.plist"];

/// Mach services and files the system resolver and TLS trust evaluation
/// use, granted only with an unrestricted network — the macOS side of
/// `launcher::profile::NETWORK_READ_PATHS`.
pub const NETWORK_MACH_SERVICES: &[&str] = &[
    "com.apple.SystemConfiguration.configd",
    "com.apple.SystemConfiguration.DNSConfiguration",
    "com.apple.trustd.agent",
];
pub const NETWORK_READ_LITERALS: &[&str] = &[
    "/private/etc/hosts",
    "/private/etc/resolv.conf",
    "/private/var/run/resolv.conf",
];
/// `getaddrinfo` asks `mDNSResponder` over this socket.
pub const RESOLVER_SOCKET: &str = "/private/var/run/mDNSResponder";

/// A rendered profile and the parameters it expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltProfile {
    pub text: String,
    pub params: Vec<(String, String)>,
}

pub fn render(policy: &SeatbeltPolicy) -> SeatbeltProfile {
    let mut out = Builder::default();
    out.baseline();
    let payload = out.param(PAYLOAD_PARAM.to_string(), &policy.payload);
    out.rule(&format!(
        "(allow file-read* file-map-executable process-exec (subpath (param \"{payload}\")))"
    ));
    let runtime = policy.runtime.as_ref().map(|dir| {
        let name = out.param(RUNTIME_PARAM.to_string(), dir);
        out.rule(&format!(
            "(allow file-read* file-map-executable (subpath (param \"{name}\")))"
        ));
        name
    });
    let container = out.param(CONTAINER_PARAM.to_string(), &policy.container);
    out.rule(&format!(
        "(allow file-read* file-write* (subpath (param \"{container}\")))"
    ));
    if let Some(dir) = &policy.darwin_cache {
        let cache = out.param(DARWIN_CACHE_PARAM.to_string(), dir);
        out.rule(&format!(
            "(allow file-read* file-write* (subpath (param \"{cache}\")))"
        ));
    }
    for (index, path) in policy.read_only.iter().enumerate() {
        let name = out.param(format!("{READ_PARAM_PREFIX}{index}"), path);
        out.rule(&format!("(allow file-read* (subpath (param \"{name}\")))"));
    }
    for (index, path) in policy.read_write.iter().enumerate() {
        let name = out.param(format!("{WRITE_PARAM_PREFIX}{index}"), path);
        out.rule(&format!(
            "(allow file-read* file-write* (subpath (param \"{name}\")))"
        ));
    }
    // What the person picked in a chooser the runtime showed: a
    // kernel-signed extension per path, consumed by the app.
    out.rule(&format!(
        "(allow file-read* file-write* (extension {}))",
        quote(USER_SELECTED_EXTENSION)
    ));
    // Path resolution stats every ancestor of a granted directory; the
    // metadata of those directories, not their listings, is what that needs.
    let roots: Vec<String> = out.params.iter().map(|(name, _)| name.clone()).collect();
    for root in roots {
        out.rule(&format!(
            "(allow file-read-metadata (path-ancestors (param \"{root}\")))"
        ));
    }
    out.network(&policy.network);

    // After every grant, so that none — a home folder, a picked path — can
    // reach Tilo's private stores or move them aside.
    out.private_stores(policy);

    // Last, so it overrides any grant above that happens to contain them.
    let mut deny = format!("(deny file-write*\n  (subpath (param \"{payload}\"))");
    if let Some(runtime) = &runtime {
        let _ = write!(deny, "\n  (subpath (param \"{runtime}\"))");
    }
    deny.push(')');
    out.rule(&deny);

    SeatbeltProfile {
        text: out.text,
        params: out.params,
    }
}

#[derive(Default)]
struct Builder {
    text: String,
    params: Vec<(String, String)>,
}

impl Builder {
    fn param(&mut self, name: String, path: &Path) -> String {
        self.params.push((name.clone(), path.display().to_string()));
        name
    }

    fn rule(&mut self, rule: &str) {
        self.text.push_str(rule);
        self.text.push('\n');
    }

    fn list(&mut self, head: &str, filter: &str, items: &[&str]) {
        self.text.push_str(head);
        for item in items {
            let _ = write!(self.text, "\n  ({filter} {})", quote(item));
        }
        self.text.push_str(")\n");
    }

    fn baseline(&mut self) {
        self.text.push_str(
            "(version 1)\n\
             (deny default)\n\
             (import \"system.sb\")\n\
             (system-graphics)\n\
             (allow process-fork)\n\
             (allow signal (target same-sandbox))\n\
             (allow system-socket (socket-domain AF_UNIX))\n\
             (allow ipc-posix-shm-read* ipc-posix-shm-write-create ipc-posix-shm-write-data\n  \
             (ipc-posix-name \"com.apple.AppleDatabaseChanged\"))\n",
        );
        self.list("(allow mach-lookup", "global-name", GUI_MACH_SERVICES);
        self.list(
            "(allow user-preference-read",
            "preference-domain",
            GUI_PREFERENCE_DOMAINS,
        );
        self.text.push_str("(allow file-read*");
        for path in GUI_READ_SUBPATHS {
            let _ = write!(self.text, "\n  (subpath {})", quote(path));
        }
        for path in GUI_READ_LITERALS {
            let _ = write!(self.text, "\n  (literal {})", quote(path));
        }
        self.text.push_str(")\n");
    }

    /// Deny every access to the [`SeatbeltPolicy::hidden`] stores, and every
    /// write — rename, removal, a new symlink target — to the
    /// [`SeatbeltPolicy::pinned`] paths themselves. A `literal` matches the
    /// path alone, so files may still be created and changed inside a
    /// pinned directory as its grant allows.
    fn private_stores(&mut self, policy: &SeatbeltPolicy) {
        if !policy.hidden.is_empty() {
            let mut rule = String::from("(deny file-read* file-write*");
            for (index, path) in policy.hidden.iter().enumerate() {
                let name = self.param(format!("{HIDDEN_PARAM_PREFIX}{index}"), path);
                let _ = write!(rule, "\n  (subpath (param \"{name}\"))");
            }
            rule.push(')');
            self.rule(&rule);
        }
        if !policy.pinned.is_empty() {
            let mut rule = String::from("(deny file-write*");
            for (index, path) in policy.pinned.iter().enumerate() {
                let name = self.param(format!("{PINNED_PARAM_PREFIX}{index}"), path);
                let _ = write!(rule, "\n  (literal (param \"{name}\"))");
            }
            rule.push(')');
            self.rule(&rule);
        }
    }

    fn network(&mut self, network: &SeatbeltNetwork) {
        match network {
            SeatbeltNetwork::Denied => {}
            SeatbeltNetwork::Gate(port) => {
                self.params
                    .push((GATE_PORT_PARAM.to_string(), port.to_string()));
                self.text
                    .push_str("(allow system-socket (socket-domain AF_INET AF_INET6))\n");
                self.rule(&format!(
                    "(allow network-outbound (remote ip (string-append \"localhost:\" (param \"{GATE_PORT_PARAM}\"))))"
                ));
            }
            SeatbeltNetwork::Unrestricted => {
                self.text.push_str(
                    "(system-network)\n\
                     (allow system-socket (socket-domain AF_INET AF_INET6))\n\
                     (allow network-bind network-inbound (local ip \"*:*\"))\n",
                );
                self.rule(&format!(
                    "(allow network-outbound (remote ip \"*:*\") (literal {}))",
                    quote(RESOLVER_SOCKET)
                ));
                self.list("(allow mach-lookup", "global-name", NETWORK_MACH_SERVICES);
                self.text.push_str("(allow file-read*");
                for path in NETWORK_READ_LITERALS {
                    let _ = write!(self.text, "\n  (literal {})", quote(path));
                }
                self.text.push_str(")\n");
            }
        }
    }
}

/// Quote a compile-time constant as an SBPL string. Only constants reach
/// this; paths travel as parameters.
fn quote(constant: &str) -> String {
    debug_assert!(!constant.contains(['"', '\\', '\n']));
    format!("\"{constant}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn policy(network: SeatbeltNetwork) -> SeatbeltPolicy {
        SeatbeltPolicy {
            payload: PathBuf::from("/Apps/so.tilo.notes"),
            runtime: Some(PathBuf::from("/Runtime/1")),
            container: PathBuf::from(
                "/Users/u/Library/Application Support/Tilo/tilo-rooms/personal/so.tilo.notes",
            ),
            darwin_cache: Some(PathBuf::from(
                "/private/var/folders/xy/abc/C/so.tilo+personal+so.tilo.notes",
            )),
            read_only: vec![PathBuf::from("/Users/u/Music")],
            read_write: vec![PathBuf::from("/Users/u/Documents/\"quoted\" (dir)")],
            network,
            hidden: vec![PathBuf::from("/Users/u/.config/tilo")],
            pinned: vec![
                PathBuf::from("/Users"),
                PathBuf::from("/Users/u"),
                PathBuf::from("/Users/u/.config"),
            ],
            not_enforced: Vec::new(),
        }
    }

    fn param<'a>(profile: &'a SeatbeltProfile, name: &str) -> &'a str {
        &profile.params.iter().find(|(k, _)| k == name).unwrap().1
    }

    #[test]
    fn the_offline_profile_is_pinned() {
        let profile = render(&policy(SeatbeltNetwork::Denied));
        let expected = include_str!("../tests/fixtures/offline.sb");
        assert_eq!(
            profile.text, expected,
            "the rendered profile changed; review the diff and update tests/fixtures/offline.sb"
        );
        let names: Vec<&str> = profile.params.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "PAYLOAD_DIR",
                "RUNTIME_DIR",
                "CONTAINER_DIR",
                "DARWIN_CACHE_DIR",
                "READ_0",
                "WRITE_0",
                "HIDDEN_0",
                "PINNED_0",
                "PINNED_1",
                "PINNED_2"
            ]
        );
    }

    #[test]
    fn it_denies_by_default_and_carries_paths_only_as_parameters() {
        for network in [
            SeatbeltNetwork::Denied,
            SeatbeltNetwork::Gate(9050),
            SeatbeltNetwork::Unrestricted,
        ] {
            let p = policy(network);
            let profile = render(&p);
            let lines: Vec<&str> = profile.text.lines().collect();
            assert_eq!(lines[0], "(version 1)");
            assert_eq!(lines[1], "(deny default)");
            assert!(!profile.text.contains("(allow default)"));
            assert!(!profile.text.contains("(allow file-read*)\n"));
            assert!(!profile.text.contains("(allow file-write*)\n"));
            for path in [
                &p.payload,
                &p.container,
                p.darwin_cache.as_ref().unwrap(),
                &p.read_only[0],
                &p.read_write[0],
            ] {
                assert!(
                    !profile.text.contains(&*path.to_string_lossy()),
                    "{} was spliced into the text",
                    path.display()
                );
            }
            assert_eq!(
                param(&profile, "WRITE_0"),
                p.read_write[0].to_str().unwrap()
            );
            assert_eq!(
                param(&profile, CONTAINER_PARAM),
                p.container.to_str().unwrap()
            );
        }
    }

    #[test]
    fn code_runs_only_from_the_payload_and_runtime_and_neither_is_writable() {
        let profile = render(&policy(SeatbeltNetwork::Denied));
        let exec: Vec<&str> = profile
            .text
            .lines()
            .filter(|l| l.contains("process-exec") || l.contains("file-map-executable"))
            .collect();
        assert_eq!(exec.len(), 2, "{exec:?}");
        assert!(exec[0].contains("(param \"PAYLOAD_DIR\")"));
        assert!(exec[0].contains("process-exec"));
        assert!(exec[1].contains("(param \"RUNTIME_DIR\")"));
        assert!(!exec[1].contains("process-exec"));
        // The container is writable and never executable.
        let container_line = profile
            .text
            .lines()
            .find(|l| l.contains("(param \"CONTAINER_DIR\")") && l.contains("file-write*"))
            .unwrap();
        assert!(!container_line.contains("exec"));
        let tail = profile.text.rsplit("(deny file-write*").next().unwrap();
        assert!(tail.contains("(param \"PAYLOAD_DIR\")"));
        assert!(tail.contains("(param \"RUNTIME_DIR\")"));
        assert!(
            !tail.contains("(allow"),
            "the write denial must be the last rule"
        );
    }

    #[test]
    fn private_stores_are_denied_after_every_grant() {
        for network in [
            SeatbeltNetwork::Denied,
            SeatbeltNetwork::Gate(9050),
            SeatbeltNetwork::Unrestricted,
        ] {
            let p = policy(network);
            let profile = render(&p);
            let hidden = profile
                .text
                .find("(deny file-read* file-write*\n  (subpath (param \"HIDDEN_0\")))")
                .expect("the store is denied outright");
            let pinned = profile
                .text
                .find(
                    "(deny file-write*\n  (literal (param \"PINNED_0\"))\n  \
                     (literal (param \"PINNED_1\"))\n  (literal (param \"PINNED_2\")))",
                )
                .expect("every directory on the way is pinned");
            // In SBPL the last matching rule wins: no allow may follow.
            assert!(
                !profile
                    .text
                    .get(hidden.min(pinned)..)
                    .unwrap()
                    .contains("(allow"),
                "a grant follows the private store denial:\n{}",
                profile.text
            );
            assert_eq!(param(&profile, "HIDDEN_0"), "/Users/u/.config/tilo");
            assert_eq!(param(&profile, "PINNED_2"), "/Users/u/.config");
            // Path ancestors of a store are not made stat-able for it.
            assert!(
                !profile
                    .text
                    .contains("(path-ancestors (param \"HIDDEN_0\"))")
            );
        }

        let mut none = policy(SeatbeltNetwork::Denied);
        none.hidden.clear();
        none.pinned.clear();
        let profile = render(&none);
        assert!(!profile.text.contains("HIDDEN_") && !profile.text.contains("PINNED_"));
    }

    #[test]
    fn network_rules_follow_the_egress_decision() {
        let offline = render(&policy(SeatbeltNetwork::Denied));
        assert!(!offline.text.contains("network-outbound"));
        assert!(!offline.text.contains("network-bind"));
        assert!(!offline.text.contains("AF_INET"));
        assert!(!offline.text.contains("(system-network)"));

        let gated = render(&policy(SeatbeltNetwork::Gate(9050)));
        let outbound: Vec<&str> = gated
            .text
            .lines()
            .filter(|l| l.contains("network"))
            .collect();
        // One loopback port, no filesystem socket, no other address.
        assert_eq!(
            outbound,
            [
                "(allow network-outbound (remote ip (string-append \"localhost:\" (param \"NET_GATE_PORT\"))))"
            ]
        );
        assert_eq!(param(&gated, GATE_PORT_PARAM), "9050");
        assert!(!gated.text.contains("(system-network)"));
        assert!(!gated.text.contains("network-bind"));

        let open = render(&policy(SeatbeltNetwork::Unrestricted));
        assert!(open.text.contains("(system-network)"));
        assert!(open.text.contains("(remote ip \"*:*\")"));
        // IP only: filesystem sockets other than the resolver stay closed.
        assert!(!open.text.contains("(allow network-outbound)"));
        assert!(!open.text.contains("(allow network*)"));
        assert!(open.text.contains(RESOLVER_SOCKET));
    }

    #[test]
    fn the_fixed_grants_name_no_user_content() {
        for path in GUI_READ_SUBPATHS
            .iter()
            .chain(GUI_READ_LITERALS)
            .chain(NETWORK_READ_LITERALS)
        {
            assert!(path.starts_with('/'), "{path}");
            assert!(!path.starts_with("/Users"), "{path} is user content");
            assert!(!path.contains('~') && !path.contains(".."), "{path}");
            assert!(
                Path::new(path).components().count() >= 3,
                "{path} is too broad"
            );
        }
        for service in GUI_MACH_SERVICES.iter().chain(NETWORK_MACH_SERVICES) {
            assert!(service.starts_with("com.apple."), "{service}");
        }
        // Services that would bypass the capability model: TCC (camera,
        // microphone, screen recording prompts), and the Tilo runtime's own
        // sockets, which reach an app only as inherited descriptors.
        let profile = render(&policy(SeatbeltNetwork::Unrestricted));
        assert!(!profile.text.contains("tccd"));
        assert!(!profile.text.contains("Tilo/runtime"));
    }
}
