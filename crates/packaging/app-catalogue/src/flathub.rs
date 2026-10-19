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

#[cfg(test)]
mod tests {
    use super::*;

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
