//! The capability vocabulary of a Tilo app and what each target provides.
//!
//! An app lists capabilities by name in `tilo-app.toml`:
//!
//! ```toml
//! [capabilities]
//! required = ["network.http"]
//! optional = ["files.save", "notifications"]
//! ```
//!
//! [`Capability::availability`] answers, per [`Target`], whether a host can
//! provide one. `tilo-app build` refuses a `required` capability its target
//! cannot provide ([`check`]), macOS packaging derives entitlements and
//! usage strings from the list, and the docs are generated from the same
//! rows. Where a platform crate implements a capability, its row is that
//! crate's own `CAPABILITIES` table, not a copy.

use std::fmt;

/// A platform an app is built for; the columns of every table here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    Linux,
    Macos,
    Web,
}

impl Target {
    pub const ALL: [Self; 3] = [Self::Linux, Self::Macos, Self::Web];

    /// The manifest spelling (`package.platforms`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Web => "web",
        }
    }

    /// Parse a manifest platform. `tilo-os` is Linux with Tilo OS host
    /// services, so it reads the Linux column.
    #[must_use]
    pub fn from_platform(name: &str) -> Option<Self> {
        match name {
            "linux" | "tilo-os" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            "web" => Some(Self::Web),
            _ => None,
        }
    }

    const fn column(self) -> usize {
        match self {
            Self::Linux => 0,
            Self::Macos => 1,
            Self::Web => 2,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a target can provide a capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    /// Usable after a user prompt (browser permission, macOS TCC).
    Prompt,
    /// Possible on this platform, but no backend is written yet.
    Planned,
    /// Never on this platform; the reason is user-facing.
    Unsupported(&'static str),
}

impl Availability {
    /// Whether an app can rely on the capability once the user agrees.
    #[must_use]
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Available | Self::Prompt)
    }
}

macro_rules! capabilities {
    ($($variant:ident = $name:literal: $summary:literal,)*) => {
        /// A host capability an app can declare.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum Capability {
            $(#[doc = $summary] $variant,)*
        }

        impl Capability {
            /// Every capability, in documentation order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The manifest spelling.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// One user-facing line on what the capability grants.
            #[must_use]
            pub fn summary(self) -> &'static str {
                match self {
                    $(Self::$variant => $summary,)*
                }
            }
        }
    };
}

capabilities! {
    Clipboard = "clipboard": "Read and write the clipboard.",
    FilesOpen = "files.open": "Open files the user picks, by contents.",
    FilesSave = "files.save": "Save contents to a place the user picks.",
    FilesPaths = "files.paths": "Work with real local file system paths.",
    AppFiles = "app-files": "Keep app-private files and settings.",
    Trash = "trash": "Move files to the user's trash.",
    OpenUri = "open-uri": "Open links in the user's browser or mail client.",
    Notifications = "notifications": "Show notifications.",
    Secrets = "secrets": "Keep secrets in the platform's credential store.",
    Settings = "settings": "Keep per-app preferences.",
    NetworkHttp = "network.http": "Make HTTP requests.",
    NetworkFetchProxy = "network.fetch-proxy": "Read any site's responses in a page through the host's fetch proxy.",
    NetworkStatus = "network.status": "Know whether the device is online or metered.",
    NetworkSockets = "network.sockets": "Open raw TCP and UDP sockets.",
    Background = "background": "Keep working while the app is open.",
    Threads = "threads": "Run work on OS threads.",
    ScheduledWake = "scheduled-wake": "Wake the app at a time while it is closed.",
    MediaDecode = "media.decode": "Decode audio and video.",
    MediaSession = "media.session": "Show now-playing controls.",
    Camera = "camera": "Use the camera.",
    Microphone = "microphone": "Use the microphone.",
    ScreenCapture = "screen-capture": "Capture the screen.",
    Speech = "speech": "Speak text aloud.",
    Spellcheck = "spellcheck": "Check spelling.",
    Location = "location": "Know the device's location.",
    Print = "print": "Print.",
    Processes = "processes": "Start and manage other programs.",
    SystemInfo = "system-info": "Read hardware and operating-system details.",
    WebView = "webview": "Embed web content.",
}

/// Where a capability's row comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The platform crate that implements it, by crate name.
    PlatformCrate(&'static str),
    /// This crate's table; no platform crate implements it yet.
    Table,
}

const WEB_SOCKETS: &str = "Browsers cannot open raw TCP or UDP sockets.";
const WEB_PROCESSES: &str = "A browser page cannot start programs.";
const WEB_WEBVIEW: &str = "A browser page cannot embed another browser.";
const WEB_WAKE: &str = "A closed browser page cannot be woken reliably.";

impl Capability {
    /// Parse a manifest spelling.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|capability| capability.name() == name)
    }

    /// The closest known name to a misspelled one, for fix-it messages.
    #[must_use]
    pub fn suggest(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .map(|capability| (edit_distance(name, capability.name()), capability))
            .filter(|(distance, _)| *distance <= 3)
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, capability)| capability)
    }

    /// Where [`availability`](Self::availability) reads this row from.
    #[must_use]
    pub fn source(self) -> Source {
        match self {
            Self::FilesPaths | Self::Trash => Source::PlatformCrate("tilo-files"),
            Self::AppFiles => Source::PlatformCrate("tilo-paths"),
            Self::Threads => Source::PlatformCrate("tilo-rt"),
            _ => Source::Table,
        }
    }

    /// Whether `target` can provide the capability. A host may report less
    /// at run time (no Secret Service in this session), never more.
    #[must_use]
    pub fn availability(self, target: Target) -> Availability {
        use Availability::{Available, Planned, Prompt, Unsupported};
        let row: [Availability; 3] = match self {
            Self::FilesPaths => {
                platform_row(tilo_files::CAPABILITIES, tilo_files::Feature::NativePaths)
            }
            Self::Trash => platform_row(tilo_files::CAPABILITIES, tilo_files::Feature::Trash),
            Self::AppFiles => both(
                platform_row(tilo_paths::CAPABILITIES, tilo_paths::Feature::AppStorage),
                platform_row(tilo_files::CAPABILITIES, tilo_files::Feature::Files),
            ),
            Self::Threads => platform_row(tilo_rt::CAPABILITIES, tilo_rt::Feature::Threads),
            Self::Processes => {
                let process =
                    platform_row(tilo_process::CAPABILITIES, tilo_process::Feature::ProcessId);
                [process[0], process[1], Unsupported(WEB_PROCESSES)]
            }
            Self::Clipboard
            | Self::FilesOpen
            | Self::OpenUri
            | Self::Settings
            | Self::NetworkHttp
            | Self::NetworkFetchProxy
            | Self::NetworkStatus
            | Self::Background
            | Self::Spellcheck
            | Self::Secrets => [Available, Available, Available],
            Self::FilesSave => [Available, Planned, Available],
            Self::Notifications => [Available, Prompt, Prompt],
            Self::Print => [Planned, Planned, Available],
            Self::NetworkSockets => [Available, Available, Unsupported(WEB_SOCKETS)],
            Self::ScheduledWake => [Planned, Planned, Unsupported(WEB_WAKE)],
            Self::MediaDecode | Self::MediaSession => [Available, Available, Planned],
            Self::Speech => [Available, Planned, Planned],
            Self::Location => [Prompt, Prompt, Prompt],
            Self::Camera | Self::Microphone | Self::ScreenCapture => [Prompt, Planned, Planned],
            Self::SystemInfo => [Planned, Planned, Planned],
            Self::WebView => [Available, Available, Unsupported(WEB_WEBVIEW)],
        };
        row.get(target.column()).copied().unwrap_or(Planned)
    }

    /// macOS entitlements the capability needs under the hardened runtime.
    #[must_use]
    pub fn macos_entitlements(self) -> &'static [&'static str] {
        match self {
            Self::NetworkHttp
            | Self::NetworkFetchProxy
            | Self::NetworkSockets
            | Self::NetworkStatus => &["com.apple.security.network.client"],
            Self::FilesOpen | Self::FilesSave | Self::FilesPaths => {
                &["com.apple.security.files.user-selected.read-write"]
            }
            Self::Camera => &["com.apple.security.device.camera"],
            Self::Microphone => &["com.apple.security.device.audio-input"],
            Self::Location => &["com.apple.security.personal-information.location"],
            Self::Print => &["com.apple.security.print"],
            _ => &[],
        }
    }

    /// The `Info.plist` usage-description key macOS requires before it
    /// prompts for the capability, with a default description.
    #[must_use]
    pub fn macos_usage_description(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Camera => Some(("NSCameraUsageDescription", "This app uses the camera.")),
            Self::Microphone => Some((
                "NSMicrophoneUsageDescription",
                "This app uses the microphone.",
            )),
            Self::Location => Some((
                "NSLocationWhenInUseUsageDescription",
                "This app uses your location.",
            )),
            Self::ScreenCapture => Some((
                "NSScreenCaptureUsageDescription",
                "This app captures the screen.",
            )),
            _ => None,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `feature`'s row in a platform crate's capability `table`.
fn platform_row<F: Copy + PartialEq>(
    table: &tilo_platform_caps::Table<F>,
    feature: F,
) -> [Availability; 3] {
    let row = table
        .iter()
        .find(|(listed, _)| *listed == feature)
        .map(|(_, row)| *row);
    row.map_or([Availability::Planned; 3], |row| {
        row.map(|availability| match availability {
            tilo_platform_caps::Availability::Available => Availability::Available,
            tilo_platform_caps::Availability::Prompt => Availability::Prompt,
            tilo_platform_caps::Availability::Unsupported(reason) => unsupported_or_planned(reason),
        })
    })
}

/// Platform crates mark features without a backend yet as unsupported with
/// this reason; for an app those are planned, not impossible.
const NOT_YET: &str = "Tilo cannot do this yet.";

fn unsupported_or_planned(reason: &'static str) -> Availability {
    if reason == NOT_YET {
        Availability::Planned
    } else {
        Availability::Unsupported(reason)
    }
}

/// A capability backed by two crates is as available as the weaker one.
fn both(left: [Availability; 3], right: [Availability; 3]) -> [Availability; 3] {
    let mut row = left;
    for (slot, other) in row.iter_mut().zip(right) {
        *slot = weaker(*slot, other);
    }
    row
}

fn weaker(left: Availability, right: Availability) -> Availability {
    fn rank(availability: Availability) -> u8 {
        match availability {
            Availability::Available => 0,
            Availability::Prompt => 1,
            Availability::Planned => 2,
            Availability::Unsupported(_) => 3,
        }
    }
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

/// One problem with an app's declared capabilities for a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// A name that is not in the vocabulary.
    Unknown {
        name: String,
        suggestion: Option<Capability>,
    },
    /// A required capability the target can never provide.
    RequiredUnsupported {
        capability: Capability,
        target: Target,
        reason: &'static str,
    },
    /// A required capability whose backend on the target is not written yet.
    RequiredPlanned {
        capability: Capability,
        target: Target,
    },
}

impl Finding {
    /// Whether the finding stops a build (`allow_planned` downgrades
    /// [`Finding::RequiredPlanned`]).
    #[must_use]
    pub fn is_error(&self, allow_planned: bool) -> bool {
        !matches!(self, Self::RequiredPlanned { .. }) || !allow_planned
    }
}

/// Check declared capabilities against a target.
///
/// Optional capabilities only have to be known names: the app degrades
/// where they are missing.
#[must_use]
pub fn check(required: &[String], optional: &[String], target: Target) -> Vec<Finding> {
    let mut findings = Vec::new();
    for name in required.iter().chain(optional) {
        if Capability::from_name(name).is_none() {
            findings.push(Finding::Unknown {
                name: name.clone(),
                suggestion: Capability::suggest(name),
            });
        }
    }
    for capability in required
        .iter()
        .filter_map(|name| Capability::from_name(name))
    {
        match capability.availability(target) {
            Availability::Available | Availability::Prompt => {}
            Availability::Planned => findings.push(Finding::RequiredPlanned { capability, target }),
            Availability::Unsupported(reason) => findings.push(Finding::RequiredUnsupported {
                capability,
                target,
                reason,
            }),
        }
    }
    findings
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, l) in left.chars().enumerate() {
        let mut current = vec![i.saturating_add(1)];
        for (j, r) in right.iter().enumerate() {
            let substitute = previous
                .get(j)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(usize::from(l != *r));
            let insert = current
                .get(j)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            let delete = previous
                .get(j.saturating_add(1))
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            current.push(substitute.min(insert).min(delete));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for capability in Capability::ALL {
            assert_eq!(Capability::from_name(capability.name()), Some(*capability));
            assert!(seen.insert(capability.name()), "{capability} listed twice");
        }
    }

    #[test]
    fn platform_crate_rows_are_read_from_the_crates() {
        assert_eq!(
            Capability::FilesPaths.availability(Target::Web),
            platform_row(tilo_files::CAPABILITIES, tilo_files::Feature::NativePaths)[2]
        );
        assert!(matches!(
            Capability::Threads.availability(Target::Web),
            Availability::Unsupported(_)
        ));
        assert_eq!(
            Capability::Trash.availability(Target::Linux),
            Availability::Planned
        );
        assert_eq!(
            Capability::AppFiles.availability(Target::Web),
            Availability::Available
        );
    }

    #[test]
    fn a_required_capability_the_web_cannot_provide_is_an_error() {
        let findings = check(&["processes".into()], &[], Target::Web);
        assert!(matches!(
            findings.as_slice(),
            [Finding::RequiredUnsupported {
                capability: Capability::Processes,
                target: Target::Web,
                ..
            }]
        ));
        assert!(check(&["processes".into()], &[], Target::Linux).is_empty());
        assert!(check(&[], &["processes".into()], Target::Web).is_empty());
    }

    #[test]
    fn planned_backends_are_errors_unless_allowed() {
        let findings = check(&["speech".into()], &[], Target::Macos);
        let [finding] = findings.as_slice() else {
            panic!("expected one finding: {findings:?}");
        };
        assert!(finding.is_error(false));
        assert!(!finding.is_error(true));
    }

    #[test]
    fn unknown_names_get_a_suggestion() {
        let findings = check(&[], &["network.htp".into()], Target::Linux);
        assert_eq!(
            findings,
            vec![Finding::Unknown {
                name: "network.htp".into(),
                suggestion: Some(Capability::NetworkHttp),
            }]
        );
    }

    #[test]
    fn tilo_os_reads_the_linux_column() {
        assert_eq!(Target::from_platform("tilo-os"), Some(Target::Linux));
        assert_eq!(Target::from_platform("windows"), None);
    }
}
