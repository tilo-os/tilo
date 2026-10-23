//! Installing a Flathub app: the same rule the app host applies, as the
//! Flatpak commands that carry it out.
//!
//! An app is installed into the person's own installation, from Flathub as
//! the device or the person configured it, and only when that source is the
//! official repository, enabled and verifying signatures.

/// The repository addresses Flathub is served from.
const OFFICIAL_URLS: [&str; 2] = ["https://dl.flathub.org/repo", "https://flathub.org/repo"];

/// Flathub's repository description, for adding the person's own source.
pub const FLATHUB_REPO_FILE: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// Why a Flathub app cannot be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallRefusal {
    /// The id could name a Flatpak option or something other than an app.
    InvalidId,
    /// The person's Flathub source is disabled, unverified or not Flathub.
    UntrustedUserSource,
    /// Neither installation has an enabled, verifying official Flathub.
    NoFlathub,
}

impl std::fmt::Display for InstallRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidId => "This is not a valid Flathub app.",
            Self::UntrustedUserSource => {
                "Your Flathub source must be enabled, verify signatures and use the official repository."
            }
            Self::NoFlathub => "Flathub is not set up on this device.",
        })
    }
}

/// Whether `id` is an application id Flatpak can only read as one.
#[must_use]
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('-')
        && id.contains('.')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The `flatpak` argument lists that install `app_id`, in order, given what
/// `flatpak remotes --columns=name,url,title,options` printed.
///
/// # Errors
///
/// [`InstallRefusal`] when the id is invalid or no trustworthy Flathub
/// source is configured.
pub fn install_commands(app_id: &str, remotes: &str) -> Result<Vec<Vec<String>>, InstallRefusal> {
    if !valid_app_id(app_id) {
        return Err(InstallRefusal::InvalidId);
    }
    require_enabled_flathub(remotes)?;
    let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    Ok(vec![
        args(&[
            "remote-add",
            "--user",
            "--if-not-exists",
            "flathub",
            FLATHUB_REPO_FILE,
        ]),
        args(&[
            "install",
            "--user",
            "--noninteractive",
            "--",
            "flathub",
            app_id,
        ]),
    ])
}

/// Which Flatpak installation an app is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Installation {
    /// The person's own (`--user`).
    User,
    /// The device's (`--system`), which only an administrator changes:
    /// Flatpak's system helper asks polkit, whose rule lets a local, active
    /// administrator through.
    System,
}

impl Installation {
    fn flag(self) -> &'static str {
        match self {
            Self::User => "--user",
            Self::System => "--system",
        }
    }
}

/// The `flatpak` argument lists that uninstall `app_id` from `installation`,
/// in order: the app, then the runtimes no app there uses any more (nothing
/// else removes those, so each removed app would leave its runtime behind
/// for good). The app's data (`~/.var/app/<id>`) is left for the caller,
/// which moves it to the Trash when the person asks; Flatpak's own
/// `--delete-data` would delete it.
///
/// # Errors
///
/// [`InstallRefusal::InvalidId`] when `app_id` is not an application id.
pub fn uninstall_commands(
    app_id: &str,
    installation: Installation,
) -> Result<Vec<Vec<String>>, InstallRefusal> {
    if !valid_app_id(app_id) {
        return Err(InstallRefusal::InvalidId);
    }
    let scope = installation.flag();
    let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    Ok(vec![
        args(&["uninstall", scope, "--noninteractive", "--", app_id]),
        args(&["uninstall", scope, "--unused", "--noninteractive"]),
    ])
}

/// An account Flathub source must itself be trustworthy; otherwise an
/// enabled, verifying official device source is enough.
fn require_enabled_flathub(rows: &str) -> Result<(), InstallRefusal> {
    let mut device_enabled = false;
    for row in rows.lines() {
        let mut columns = row.split('\t');
        if columns.next() != Some("flathub") {
            continue;
        }
        let url = columns.next().unwrap_or_default().trim_end_matches('/');
        let _title = columns.next();
        let options: Vec<&str> = columns
            .next()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .collect();
        let official = OFFICIAL_URLS.contains(&url);
        let enabled = !options.contains(&"disabled") && !options.contains(&"no-gpg-verify");
        if !options.contains(&"system") {
            return if official && enabled {
                Ok(())
            } else {
                Err(InstallRefusal::UntrustedUserSource)
            };
        }
        device_enabled |= official && enabled;
    }
    if device_enabled {
        Ok(())
    } else {
        Err(InstallRefusal::NoFlathub)
    }
}

/// The processor whose Flathub builds are looked up beside this machine's:
/// `x86_64` on every other machine, since hundreds of Flathub apps (Steam,
/// Spotify, Discord, Zoom) are built for it alone. `None` on `x86_64`.
#[must_use]
pub fn other_processor() -> Option<&'static str> {
    (std::env::consts::ARCH != "x86_64").then_some("x86_64")
}

/// The `flatpak` arguments that list the device's Flathub apps for `arch`
/// from the summary Flatpak keeps (no network), as `id<TAB>name` rows. The
/// hourly `tilo-app-update` run keeps that summary current.
#[must_use]
pub fn listing_args(arch: &str) -> Vec<String> {
    [
        "remote-ls",
        "--system",
        "--cached",
        "--app",
        "--arch",
        arch,
        "--columns=application,name",
        "--",
        crate::appstream::FLATHUB_REMOTE,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The apps of `rows` ([`listing_args`]' output for another processor) that
/// `native` (this processor's catalogue) does not list: Flathub builds them
/// only for that processor, so each is marked
/// [`crate::CatalogueEntry::not_built_for_this_device`]. It is found and
/// says it is not available here, and is never offered for installing.
#[must_use]
pub fn other_processor_apps(
    rows: &str,
    native: &[crate::CatalogueEntry],
) -> Vec<crate::CatalogueEntry> {
    let mut seen: std::collections::HashSet<String> =
        native.iter().map(crate::CatalogueEntry::key).collect();
    rows.lines()
        .filter_map(|row| {
            let mut columns = row.split('\t');
            let id = columns.next()?.trim();
            let name = columns
                .next()
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            (valid_app_id(id) && seen.insert(id.to_ascii_lowercase())).then(|| {
                crate::CatalogueEntry {
                    id: id.to_owned(),
                    source: crate::Source::Flathub {
                        remote: crate::appstream::FLATHUB_REMOTE.to_owned(),
                        installation: crate::Installation::System,
                    },
                    name: name.to_owned(),
                    other_names: Vec::new(),
                    tagline: String::new(),
                    description: String::new(),
                    keywords: Vec::new(),
                    category: None,
                    icon_file: None,
                    icon_url: None,
                    resource_types: Vec::new(),
                    actions: Vec::new(),
                    not_built_for_this_device: true,
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uninstalling touches only the person's installation, never deletes
    /// the app's data (`--delete-data`), and refuses anything that could be
    /// read as an option.
    #[test]
    fn uninstalling_removes_the_app_from_its_installation_and_keeps_its_data() {
        for (installation, scope) in [
            (Installation::User, "--user"),
            (Installation::System, "--system"),
        ] {
            let commands = uninstall_commands("org.gnome.Chess", installation).expect("valid id");
            assert_eq!(
                commands,
                [
                    vec![
                        "uninstall",
                        scope,
                        "--noninteractive",
                        "--",
                        "org.gnome.Chess"
                    ],
                    vec!["uninstall", scope, "--unused", "--noninteractive"],
                ]
            );
        }
        for bad in ["--delete-data", "--all", "chess", "", "org.x/../y"] {
            assert_eq!(
                uninstall_commands(bad, Installation::User),
                Err(InstallRefusal::InvalidId),
                "{bad}"
            );
        }
    }

    /// Flathub builds Steam for `x86_64` only. Listed from the other
    /// processor's summary it is an entry marked not built for this device;
    /// an app this processor's catalogue already has is not listed twice,
    /// and a row that could name a Flatpak option is dropped.
    #[test]
    fn apps_only_another_processor_has_are_marked_not_built_for_this_device() {
        let mut spotify = crate::tilo::entry(&registry_types::catalogue::CatalogueApp {
            package_id: "com.spotify.Client".into(),
            display_name: "Spotify".into(),
            ..Default::default()
        });
        spotify.source = crate::Source::Flathub {
            remote: "flathub".into(),
            installation: crate::Installation::System,
        };
        let rows = "com.valvesoftware.Steam\tSteam\ncom.spotify.Client\tSpotify\n--system\tEvil\norg.example.Nameless\t\n";
        let apps = other_processor_apps(rows, &[spotify]);
        assert_eq!(apps.len(), 1, "{apps:?}");
        assert_eq!(apps[0].id, "com.valvesoftware.Steam");
        assert_eq!(apps[0].name, "Steam");
        assert!(apps[0].not_built_for_this_device);
        assert!(apps[0].source.needs_install());
        assert_eq!(
            listing_args("x86_64").last().map(String::as_str),
            Some("flathub")
        );
        assert!(listing_args("x86_64").iter().any(|arg| arg == "--cached"));
    }

    #[test]
    fn only_a_trustworthy_flathub_installs_and_only_into_the_persons_installation() {
        let device = "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\n";
        let commands = install_commands("com.spotify.Client", device).expect("installs");
        assert_eq!(
            commands.last().map(Vec::as_slice),
            Some(
                [
                    "install",
                    "--user",
                    "--noninteractive",
                    "--",
                    "flathub",
                    "com.spotify.Client"
                ]
                .map(str::to_owned)
                .as_slice()
            )
        );
        assert_eq!(
            install_commands("--system", device),
            Err(InstallRefusal::InvalidId)
        );
        assert_eq!(
            install_commands(
                "com.spotify.Client",
                "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem,no-gpg-verify\n"
            ),
            Err(InstallRefusal::NoFlathub)
        );
        assert_eq!(
            install_commands(
                "com.spotify.Client",
                "flathub\thttps://evil.example/repo\tFlathub\t\n"
            ),
            Err(InstallRefusal::UntrustedUserSource)
        );
    }
}
