//! Consented source discovery. Presence inspection uses private bounded snapshots;
//! keychains and decryption are reserved for selected capture.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Cookies,
    DiscordOrigin,
}
pub struct CookieProfile {
    pub id: String,
    pub label: String,
    /// Public application metadata from the consented discovery registry.
    pub application: String,
    pub native_application: bool,
    pub provider: Provider,
    pub material: Material,
    directory: Arc<PinnedDirectory>,
    native_slack: bool,
    slack_application:
        Option<Arc<crate::setup::service_sessions::source::slack_linux::SlackApplication>>,
    brand: Option<crate::setup::service_sessions::source::chromium_linux::ChromiumBrand>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Readable,
    NeedsAccess,
    Unavailable,
}
impl CookieProfile {
    /// Called only by the migration-consented inventory worker. Returns no cookie
    /// value, origin value, source digest, or credential-derived identifier.
    pub fn presence(&self, service: browser_continuity::sessions::Service) -> io::Result<Presence> {
        use crate::setup::service_sessions::source::{self, SourceError};
        use browser_continuity::sessions::Service;
        self.validate()?;
        if self.native_slack && service != Service::Slack {
            return Ok(Presence::Unavailable);
        }
        let result = match self.material {
            Material::DiscordOrigin if service == Service::Discord => {
                source::discord_origin::read_selected(&self.origin_grant()?).map(drop)
            }
            Material::DiscordOrigin => return Ok(Presence::Unavailable),
            Material::Cookies => {
                // Deliberately omit the selected-capture decryptor. This cannot
                // contact Secret Service, even when the profile has v11 cookies.
                let grant = source::SourceGrant::new(
                    super::super::migration_durability::readable_directory(&self.directory.file)?,
                    self.provider,
                )
                .map_err(invalid)?;
                source::read_selected(&grant, service).map(drop)
            }
        };
        self.validate()?;
        Ok(match result {
            Ok(()) => Presence::Readable,
            Err(SourceError::UnsupportedEncryption)
                if self.brand.is_some() || self.native_slack =>
            {
                Presence::NeedsAccess
            }
            Err(_) => Presence::Unavailable,
        })
    }
    /// Source-brand capability only; does not inspect or query credentials.
    pub fn supports_encrypted_cookie_capture(&self) -> bool {
        self.material == Material::Cookies
            && (self.brand.is_some()
                || self
                    .slack_application
                    .as_ref()
                    .is_some_and(|proof| proof.validate().is_ok()))
    }
    pub fn validate(&self) -> io::Result<()> {
        self.directory.validate()?;
        storage(&self.directory)?.validate_namespace_control()?;
        if let Some(proof) = &self.slack_application {
            proof.validate().map_err(invalid)?;
        }
        Ok(())
    }
    pub fn origin_grant(
        &self,
    ) -> io::Result<crate::setup::service_sessions::source::discord_origin::DiscordOriginGrant>
    {
        self.validate()?;
        if self.material != Material::DiscordOrigin {
            return Err(invalid("Website source changed"));
        }
        crate::setup::service_sessions::source::discord_origin::DiscordOriginGrant::new(
            super::super::migration_durability::readable_directory(&self.directory.file)?,
        )
        .map_err(invalid)
    }
    pub fn grant(&self) -> io::Result<crate::setup::service_sessions::source::SourceGrant> {
        self.validate()?;
        let mut grant = crate::setup::service_sessions::source::SourceGrant::new(
            super::super::migration_durability::readable_directory(&self.directory.file)?,
            self.provider,
        )
        .map_err(invalid)?;
        if let Some(brand) = self.brand {
            // Admission does not query the keychain. The adapter reads only when
            // an explicitly selected encrypted cookie actually requires v11.
            if let Ok(adapter) =
                crate::setup::service_sessions::source::chromium_linux::ChromiumLinux::for_grant(
                    &grant, brand,
                )
            {
                grant = grant
                    .with_chromium_decryptor(Arc::new(adapter))
                    .map_err(invalid)?;
            }
        }
        if let Some(proof) = &self.slack_application {
            let adapter =
                crate::setup::service_sessions::source::slack_linux::SlackLinux::for_grant(
                    &grant,
                    Arc::clone(proof),
                )
                .map_err(invalid)?;
            grant = grant
                .with_chromium_decryptor(Arc::new(adapter))
                .map_err(invalid)?;
        }
        self.validate()?;
        Ok(grant)
    }
}
pub struct Discovery {
    home: Arc<PinnedDirectory>,
    pub sources: Vec<Arc<CookieProfile>>,
    pub unavailable: bool,
}
impl Discovery {
    pub fn validate_home(&self) -> io::Result<()> {
        self.home.validate()?;
        storage(&self.home)?.validate_namespace_control()
    }
}
pub fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Discovery> {
    discover_with_package(home, candidates, &|candidate| {
        if candidate.provider != AccountProvider::Slack {
            return None;
        }
        if candidate
            .profile
            .strip_prefix(home)
            .ok()
            .is_some_and(|path| path.starts_with(".var/app/com.slack.Slack"))
        {
            crate::setup::service_sessions::source::slack_locator::flatpak(home)
        } else {
            crate::setup::service_sessions::source::slack_locator::native()
        }
    })
}
#[cfg(any(test, feature = "test-support"))]
pub fn discover_without_slack_package(
    home: &Path,
    candidates: &[Candidate],
) -> io::Result<Discovery> {
    discover_with_package(home, candidates, &|_| None)
}
pub fn discover_with_package(
    home: &Path,
    candidates: &[Candidate],
    package: &impl Fn(
        &Candidate,
    ) -> Option<
        Arc<crate::setup::service_sessions::source::slack_linux::SlackApplication>,
    >,
) -> io::Result<Discovery> {
    // A failed Home grant is global. Individual candidate failures cannot grant
    // anything, and must not hide independent sources under this held Home.
    let root = PinnedDirectory::open_absolute(home)?;
    storage(&root)?.validate_namespace_control()?;
    let mut discovered = Discovery {
        home: Arc::clone(&root),
        sources: Vec::new(),
        unavailable: false,
    };
    for candidate in candidates.iter().filter(|candidate| {
        candidate.origin == Origin::Browser
            || matches!(
                candidate.provider,
                AccountProvider::Discord | AccountProvider::Slack
            )
    }) {
        match discover_candidate(
            home,
            std::slice::from_ref(candidate),
            &mut discovered.unavailable,
            package,
        ) {
            Ok(sources) => {
                for source in sources {
                    if discovered.sources.iter().any(|prior| prior.id == source.id) {
                        continue;
                    }
                    if discovered.sources.len() >= browser_continuity::MAX_PROFILES {
                        return Err(io::Error::new(
                            io::ErrorKind::FileTooLarge,
                            "Too many browser sources",
                        ));
                    }
                    discovered.sources.push(source);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::FileTooLarge => return Err(error),
            Err(_) => discovered.unavailable = true,
        }
        root.validate()?;
        storage(&root)?.validate_namespace_control()?;
    }
    root.validate()?;
    Ok(discovered)
}
fn discover_candidate(
    home: &Path,
    candidates: &[Candidate],
    unavailable: &mut bool,
    package: &impl Fn(
        &Candidate,
    ) -> Option<
        Arc<crate::setup::service_sessions::source::slack_linux::SlackApplication>,
    >,
) -> io::Result<Vec<Arc<CookieProfile>>> {
    let root = PinnedDirectory::open_absolute(home)?;
    storage(&root)?.validate_namespace_control()?;
    let mut output = Vec::new();
    for candidate in candidates.iter().filter(|candidate| {
        candidate.origin == Origin::Browser
            || matches!(
                candidate.provider,
                AccountProvider::Discord | AccountProvider::Slack
            )
    }) {
        let relative = components(candidate.profile.strip_prefix(home).map_err(invalid)?)?;
        let directory = walk(&root, &relative)?;
        let profiles = match candidate.provider {
            AccountProvider::Chromium => chromium_names(&directory)?,
            AccountProvider::Firefox => firefox_names(&directory)?,
            // These are exact owner-reviewed Electron user-data roots emitted
            // by account discovery, including its fixed Flatpak locations.
            // Their Cookies/Network/Cookies format uses the canonical Chromium
            // reader; this does not authorize a Chrome keychain for Electron.
            AccountProvider::Discord | AccountProvider::Slack => {
                vec![(candidate.label.clone(), Vec::new(), Provider::Chromium)]
            }
            _ => return Err(invalid("Unsupported browser source")),
        };
        for (label, names, provider) in profiles {
            let profile = if let Ok(profile) = walk(&directory, &names) {
                profile
            } else {
                *unavailable = true;
                continue;
            };
            let native_slack = candidate.provider == AccountProvider::Slack;
            let stores = (|| -> io::Result<(bool, bool)> {
                let present = match provider {
                    Provider::Firefox => regular(&profile, "cookies.sqlite")?,
                    Provider::Chromium => chromium_store(&profile)?,
                };
                let origin = !native_slack
                    && provider == Provider::Chromium
                    && discord_origin_store(&profile)?;
                Ok((present, origin))
            })();
            let (present, origin_present) = if let Ok(stores) = stores {
                stores
            } else {
                *unavailable = true;
                continue;
            };
            if !present && !origin_present {
                continue;
            }
            let mut path = relative.clone();
            path.extend(names);
            let id = blake3::hash(&serde_json::to_vec(&(provider, path)).map_err(invalid)?)
                .to_hex()
                .to_string();
            if output
                .iter()
                .any(|prior: &Arc<CookieProfile>| prior.id == id)
            {
                continue;
            }
            if output.len() == browser_continuity::MAX_PROFILES {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "Too many browser sources",
                ));
            }
            // Labels are the fixed upstream product metadata emitted by the
            // consented discovery registry, never inferred from a folder name.
            let brand = match (candidate.provider, candidate.label.as_str()) {
                (AccountProvider::Chromium, "Google Chrome") => Some(
                    crate::setup::service_sessions::source::chromium_linux::ChromiumBrand::Chrome,
                ),
                (AccountProvider::Chromium, "Chromium") => Some(
                    crate::setup::service_sessions::source::chromium_linux::ChromiumBrand::Chromium,
                ),
                _ => None,
            };
            if present && candidate.provider != AccountProvider::Discord {
                output.push(Arc::new(CookieProfile {
                    id: id.clone(),
                    label: format!("{} · {label}", candidate.label),
                    application: candidate.label.clone(),
                    native_application: native_slack,
                    provider,
                    material: Material::Cookies,
                    directory: Arc::clone(&profile),
                    native_slack,
                    slack_application: if native_slack {
                        package(candidate)
                    } else {
                        None
                    },
                    brand,
                }));
            }
            if origin_present {
                if output.len() >= browser_continuity::MAX_PROFILES {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "Too many browser sources",
                    ));
                }
                output.push(Arc::new(CookieProfile {
                    id: format!("{id}-discord-origin"),
                    label: format!("{} · {label}", candidate.label),
                    application: candidate.label.clone(),
                    native_application: candidate.provider == AccountProvider::Discord,
                    provider,
                    material: Material::DiscordOrigin,
                    directory: profile,
                    native_slack: false,
                    slack_application: None,
                    brand: None,
                }));
            }
        }
    }
    root.validate()?;
    Ok(output)
}
fn discord_origin_store(profile: &Arc<PinnedDirectory>) -> io::Result<bool> {
    let local = match metadata_nofollow(&profile.file, Path::new("Local Storage")) {
        Ok(metadata) => profile.open_child("Local Storage".as_ref(), &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    storage(&local)?.validate_namespace_control()?;
    match metadata_nofollow(&local.file, Path::new("leveldb")) {
        Ok(metadata) => {
            let store = local.open_child("leveldb".as_ref(), &metadata)?;
            storage(&store)?.validate_namespace_control()?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn chromium_store(profile: &Arc<PinnedDirectory>) -> io::Result<bool> {
    match metadata_nofollow(&profile.file, Path::new("Network")) {
        Ok(metadata) => {
            let network = profile.open_child("Network".as_ref(), &metadata)?;
            storage(&network)?.validate_namespace_control()?;
            regular(&network, "Cookies")
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => regular(profile, "Cookies"),
        Err(error) => Err(error),
    }
}
fn chromium_names(root: &Arc<PinnedDirectory>) -> io::Result<Vec<(String, Vec<String>, Provider)>> {
    let entries = cap_std::fs::Dir::from_std_file(
        super::super::migration_durability::readable_directory(&root.file)?,
    );
    let mut names = Vec::new();
    for (index, entry) in entries.entries()?.enumerate() {
        if index >= MAX_PROFILE_DIRECTORIES {
            return Err(invalid("Too many browser source directories"));
        }
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name != "Default" && !name.starts_with("Profile ") {
            continue;
        }
        names.push((name.to_owned(), vec![name.to_owned()], Provider::Chromium));
    }
    Ok(names)
}
fn firefox_names(root: &Arc<PinnedDirectory>) -> io::Result<Vec<(String, Vec<String>, Provider)>> {
    let mut file = storage(root)?.open_read("profiles.ini".as_ref(), Privacy::Inherit)?;
    if file.metadata()?.len() > MAX_INI {
        return Err(invalid("Browser source index is too large"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_INI + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INI {
        return Err(invalid("Browser source index is too large"));
    }
    firefox_sections(std::str::from_utf8(&bytes).map_err(invalid)?)?
        .into_iter()
        .map(|(label, path)| Ok((label, components(Path::new(&path))?, Provider::Firefox)))
        .collect()
}
