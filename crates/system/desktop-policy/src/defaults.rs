use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A resource an application may handle. The prefix is part of the identity,
/// so a MIME, URI scheme, and extension can never collide.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceKind(String);

impl ResourceKind {
    /// # Errors
    ///
    /// Returns [`DefaultAppError::InvalidResource`] if `ext` is empty
    /// (after trimming and stripping a leading `.`), longer than 64
    /// characters, or contains anything but letters, digits, `+`, `-` or
    /// `_`.
    pub fn extension(ext: &str) -> Result<Self, DefaultAppError> {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty()
            || ext.len() > 64
            || !ext
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_'))
        {
            return Err(DefaultAppError::InvalidResource);
        }
        Ok(Self(format!("ext:{ext}")))
    }

    /// # Errors
    ///
    /// Returns [`DefaultAppError::InvalidResource`] if `mime` has no `/`,
    /// is longer than 255 characters, either side of the `/` is empty,
    /// contains an invalid MIME token character, contains a second `/`, or
    /// either side is a bare `*` wildcard.
    pub fn mime(mime: &str) -> Result<Self, DefaultAppError> {
        let mime = mime.trim().to_ascii_lowercase();
        let Some((top, subtype)) = mime.split_once('/') else {
            return Err(DefaultAppError::InvalidResource);
        };
        if mime.len() > 255
            || subtype.contains('/')
            || !valid_mime_token(top)
            || !valid_mime_token(subtype)
            || top == "*"
            || subtype == "*"
        {
            return Err(DefaultAppError::InvalidResource);
        }
        Ok(Self(format!("mime:{mime}")))
    }

    /// # Errors
    ///
    /// Returns [`DefaultAppError::InvalidResource`] if `scheme` is empty
    /// (after trimming and stripping a trailing `:`), longer than 32
    /// characters, does not start with a letter, contains anything but
    /// letters, digits, `+`, `-` or `.`, or names a reserved scheme
    /// (`file`, `data`, `javascript`).
    pub fn scheme(scheme: &str) -> Result<Self, DefaultAppError> {
        let scheme = scheme.trim().trim_end_matches(':').to_ascii_lowercase();
        let mut chars = scheme.chars();
        if scheme.is_empty()
            || scheme.len() > 32
            || !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            || matches!(scheme.as_str(), "file" | "data" | "javascript")
        {
            return Err(DefaultAppError::InvalidResource);
        }
        Ok(Self(format!("scheme:{scheme}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn extension_name(&self) -> Option<&str> {
        self.0.strip_prefix("ext:")
    }

    pub fn mime_name(&self) -> Option<&str> {
        self.0.strip_prefix("mime:")
    }

    pub fn scheme_name(&self) -> Option<&str> {
        self.0.strip_prefix("scheme:")
    }

    /// The key `mimeapps.list` and `MimeType=` use for this resource: the MIME type
    /// itself, or `x-scheme-handler/<scheme>`. Extensions have none.
    pub fn mimeapps_type(&self) -> Option<String> {
        self.mime_name()
            .map(str::to_string)
            .or_else(|| self.scheme_name().map(|s| format!("x-scheme-handler/{s}")))
    }

    /// The inverse of [`Self::mimeapps_type`]: a `MimeType=` entry as a resource.
    pub fn from_mimeapps_type(value: &str) -> Option<Self> {
        let value = value.trim();
        match value.to_ascii_lowercase().strip_prefix("x-scheme-handler/") {
            Some(scheme) => Self::scheme(scheme).ok(),
            None => Self::mime(value).ok(),
        }
    }

    pub fn display_label(&self) -> String {
        if let Some(ext) = self.extension_name() {
            format!(".{ext} files")
        } else if let Some(mime) = self.mime_name() {
            mime.to_string()
        } else if let Some(scheme) = self.scheme_name() {
            format!("{scheme}: links")
        } else {
            self.0.clone()
        }
    }
}

fn valid_mime_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 127
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppCandidate {
    pub package_id: String,
    pub display_name: String,
    pub resources: BTreeSet<ResourceKind>,
    /// Picked for these resources on purpose rather than opened by default
    /// (an editor that imports video): without an explicit choice, another
    /// app claiming the resource wins. The manifest's `[providers].secondary`.
    pub secondary: bool,
}

impl AppCandidate {
    pub fn handles(&self, resource: &ResourceKind) -> bool {
        self.resources.contains(resource)
    }

    /// A host desktop entry rather than a Tilo package; see [`crate::mimeapps`].
    pub fn is_host(&self) -> bool {
        crate::mimeapps::host_desktop_id(&self.package_id).is_some()
    }
}

/// The administrator's choice for a workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "app")]
pub enum WorkspaceDefaultPolicy {
    /// No workspace preference: resolve through each person's personal default.
    #[default]
    FollowPersonal,
    App(String),
}

/// A person's choice inside one workspace. `FollowWorkspace` is the absent/default
/// state: it follows an admin app when set and otherwise reaches personal defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "app")]
pub enum UserWorkspaceChoice {
    #[default]
    FollowWorkspace,
    PersonalDefault,
    App(String),
}

/// The settings section holding a person's [`PersonalDefaults`].
pub const PERSONAL_DEFAULTS_SECTION: &str = "default_apps";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalDefaults {
    #[serde(default)]
    pub handlers: BTreeMap<ResourceKind, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDefaults {
    #[serde(default)]
    pub policy: BTreeMap<ResourceKind, WorkspaceDefaultPolicy>,
    #[serde(default)]
    pub user_choices: BTreeMap<String, BTreeMap<ResourceKind, UserWorkspaceChoice>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultAppSelection {
    pub package_id: String,
    pub source: SelectionSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionSource {
    UserWorkspaceOverride,
    PersonalOverride,
    WorkspacePolicy,
    PersonalDefault,
    /// The host's `mimeapps.list` chain named this host application.
    SystemDefault,
    DeterministicFallback,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DefaultAppError {
    #[error("invalid resource kind")]
    InvalidResource,
    #[error("invalid package id")]
    InvalidPackage,
    #[error("{package_id} does not claim {resource}")]
    DoesNotHandle {
        package_id: String,
        resource: String,
    },
    #[error("the application is not installed")]
    NotInstalled,
    #[error("could not persist defaults: {0}")]
    Persist(String),
}

pub struct DefaultAppResolver<'a> {
    candidates: &'a [AppCandidate],
    system_defaults: Option<&'a crate::mimeapps::MimeAppsChain>,
}

impl<'a> DefaultAppResolver<'a> {
    pub fn new(candidates: &'a [AppCandidate]) -> Self {
        Self {
            candidates,
            system_defaults: None,
        }
    }

    /// Consult the host's `mimeapps.list` chain when choosing among host applications that
    /// no personal or workspace choice names.
    #[must_use]
    pub fn with_system_defaults(mut self, chain: &'a crate::mimeapps::MimeAppsChain) -> Self {
        self.system_defaults = Some(chain);
        self
    }

    /// # Errors
    ///
    /// Returns [`DefaultAppError::InvalidPackage`] if `package_id` fails
    /// [`validate_package_id`], [`DefaultAppError::NotInstalled`] if it
    /// names no candidate, or [`DefaultAppError::DoesNotHandle`] if the
    /// candidate does not claim `resource`.
    pub fn validate_app(
        &self,
        resource: &ResourceKind,
        package_id: &str,
    ) -> Result<(), DefaultAppError> {
        validate_package_id(package_id)?;
        let Some(app) = self.candidates.iter().find(|a| a.package_id == package_id) else {
            return Err(DefaultAppError::NotInstalled);
        };
        if !app.handles(resource) {
            return Err(DefaultAppError::DoesNotHandle {
                package_id: package_id.to_string(),
                resource: resource.as_str().to_string(),
            });
        }
        Ok(())
    }

    /// Resolve in strict precedence order. Stale/uninstalled selections are
    /// skipped rather than becoming a denial-of-open after an uninstall.
    pub fn resolve(
        &self,
        resource: &ResourceKind,
        user_id: &str,
        personal: &PersonalDefaults,
        workspace: Option<&WorkspaceDefaults>,
    ) -> Option<DefaultAppSelection> {
        let valid = |id: &str| self.validate_app(resource, id).is_ok();
        let personal_id = || personal.handlers.get(resource).filter(|id| valid(id));

        if let Some(ws) = workspace {
            let choice = ws
                .user_choices
                .get(user_id)
                .and_then(|m| m.get(resource))
                .unwrap_or(&UserWorkspaceChoice::FollowWorkspace);
            match choice {
                UserWorkspaceChoice::App(id) if valid(id) => {
                    return Some(DefaultAppSelection {
                        package_id: id.clone(),
                        source: SelectionSource::UserWorkspaceOverride,
                    });
                }
                UserWorkspaceChoice::PersonalDefault => {
                    if let Some(id) = personal_id() {
                        return Some(DefaultAppSelection {
                            package_id: id.clone(),
                            source: SelectionSource::PersonalOverride,
                        });
                    }
                    // An explicit escape from workspace policy remains an
                    // escape when the personal app has since been removed.
                    // Use the safe installed fallback below instead.
                }
                _ => {}
            }
            if matches!(choice, UserWorkspaceChoice::FollowWorkspace) {
                if let Some(WorkspaceDefaultPolicy::App(id)) = ws.policy.get(resource)
                    && valid(id)
                {
                    return Some(DefaultAppSelection {
                        package_id: id.clone(),
                        source: SelectionSource::WorkspacePolicy,
                    });
                }
            } else {
                return self.fallback(resource);
            }
        }
        if let Some(id) = personal_id() {
            return Some(DefaultAppSelection {
                package_id: id.clone(),
                source: SelectionSource::PersonalDefault,
            });
        }
        self.fallback(resource)
    }

    /// Without an explicit choice: a Tilo package first, since it runs sandboxed and
    /// collaborative, one that opens the resource ahead of one that is only picked
    /// for it; then the host's own `mimeapps.list` default; then any host
    /// application, deterministically by name.
    fn fallback(&self, resource: &ResourceKind) -> Option<DefaultAppSelection> {
        let mut candidates: Vec<&AppCandidate> = self
            .candidates
            .iter()
            .filter(|app| app.handles(resource))
            .collect();
        candidates.sort_by(|a, b| {
            a.is_host()
                .cmp(&b.is_host())
                .then_with(|| a.secondary.cmp(&b.secondary))
                .then_with(|| a.display_name.cmp(&b.display_name))
                .then_with(|| a.package_id.cmp(&b.package_id))
        });
        let first = candidates.first()?;
        if !first.is_host() {
            return Some(DefaultAppSelection {
                package_id: first.package_id.clone(),
                source: SelectionSource::DeterministicFallback,
            });
        }
        if let (Some(chain), Some(mime)) = (self.system_defaults, resource.mimeapps_type())
            && let Some(id) = chain.default_for(&mime, |id| {
                candidates.iter().any(|app| app.package_id == id)
            })
        {
            return Some(DefaultAppSelection {
                package_id: id.to_string(),
                source: SelectionSource::SystemDefault,
            });
        }
        Some(DefaultAppSelection {
            package_id: first.package_id.clone(),
            source: SelectionSource::DeterministicFallback,
        })
    }
}

impl PersonalDefaults {
    /// # Errors
    ///
    /// Returns an error if [`DefaultAppResolver::validate_app`] rejects
    /// `package_id` for `resource`; the existing handler is left untouched.
    pub fn set(
        &mut self,
        resource: ResourceKind,
        package_id: String,
        resolver: &DefaultAppResolver<'_>,
    ) -> Result<(), DefaultAppError> {
        resolver.validate_app(&resource, &package_id)?;
        self.handlers.insert(resource, package_id);
        Ok(())
    }

    /// The person's defaults, from their settings: the
    /// [`PERSONAL_DEFAULTS_SECTION`] section, which follows them to each of
    /// their devices.
    #[cfg(feature = "host-storage")]
    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section(PERSONAL_DEFAULTS_SECTION))
    }

    /// Write these defaults to the person's settings.
    ///
    /// # Errors
    ///
    /// Returns [`DefaultAppError::Persist`] if the settings could not be
    /// written.
    #[cfg(feature = "host-storage")]
    pub fn save(&self) -> Result<(), DefaultAppError> {
        let serde_json::Value::Object(section) =
            serde_json::to_value(self).map_err(|e| DefaultAppError::Persist(e.to_string()))?
        else {
            return Err(DefaultAppError::Persist(
                "defaults are not an object".into(),
            ));
        };
        settings_store::update_section(PERSONAL_DEFAULTS_SECTION, section)
            .map_err(|e| DefaultAppError::Persist(e.to_string()))
    }

    /// The defaults a settings section holds; missing or malformed reads as
    /// none chosen.
    pub fn from_section(section: &serde_json::Map<String, serde_json::Value>) -> Self {
        serde_json::from_value(serde_json::Value::Object(section.clone())).unwrap_or_default()
    }
}

/// # Errors
///
/// Returns [`DefaultAppError::InvalidPackage`] if `id` is empty, longer
/// than 255 characters, starts or ends with `.`, contains `..`, or
/// contains anything but letters, digits, `.`, `-` or `_`.
pub fn validate_package_id(id: &str) -> Result<(), DefaultAppError> {
    if id.is_empty()
        || id.len() > 255
        || id.starts_with('.')
        || id.ends_with('.')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Err(DefaultAppError::InvalidPackage)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(ext: &str) -> ResourceKind {
        ResourceKind::extension(ext).unwrap()
    }
    fn app(id: &str, name: &str, ext: &str) -> AppCandidate {
        AppCandidate {
            package_id: id.into(),
            display_name: name.into(),
            resources: [r(ext)].into(),
            secondary: false,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn default_selection_follows_exact_precedence_and_falls_through_safely() {
        // Phase `exact_precedence_and_escape_hatches`.
        {
            let pdf = r("pdf");
            let apps = [
                app("so.tilo.a", "A", "pdf"),
                app("so.tilo.b", "B", "pdf"),
                app("so.tilo.c", "C", "pdf"),
            ];
            let resolver = DefaultAppResolver::new(&apps);
            let personal = PersonalDefaults {
                handlers: [(pdf.clone(), "so.tilo.a".into())].into(),
            };
            let mut ws = WorkspaceDefaults::default();

            assert_eq!(
                resolver
                    .resolve(&pdf, "ada", &personal, Some(&ws))
                    .unwrap()
                    .package_id,
                "so.tilo.a"
            );
            ws.policy
                .insert(pdf.clone(), WorkspaceDefaultPolicy::App("so.tilo.b".into()));
            assert_eq!(
                resolver
                    .resolve(&pdf, "ada", &personal, Some(&ws))
                    .unwrap()
                    .package_id,
                "so.tilo.b"
            );
            ws.user_choices
                .entry("ada".into())
                .or_default()
                .insert(pdf.clone(), UserWorkspaceChoice::PersonalDefault);
            assert_eq!(
                resolver
                    .resolve(&pdf, "ada", &personal, Some(&ws))
                    .unwrap()
                    .package_id,
                "so.tilo.a"
            );
            ws.user_choices
                .get_mut("ada")
                .unwrap()
                .insert(pdf.clone(), UserWorkspaceChoice::App("so.tilo.c".into()));
            assert_eq!(
                resolver
                    .resolve(&pdf, "ada", &personal, Some(&ws))
                    .unwrap()
                    .package_id,
                "so.tilo.c"
            );
            ws.policy
                .insert(pdf.clone(), WorkspaceDefaultPolicy::FollowPersonal);
            ws.user_choices
                .get_mut("ada")
                .unwrap()
                .insert(pdf.clone(), UserWorkspaceChoice::FollowWorkspace);
            assert_eq!(
                resolver
                    .resolve(&pdf, "ada", &personal, Some(&ws))
                    .unwrap()
                    .package_id,
                "so.tilo.a"
            );
        }
        // Phase `invalid_or_uninstalled_selection_falls_through_safely`.
        {
            let pdf = r("pdf");
            let apps = [app("so.tilo.viewer", "Viewer", "pdf")];
            let resolver = DefaultAppResolver::new(&apps);
            let personal = PersonalDefaults {
                handlers: [(pdf.clone(), "so.tilo.removed".into())].into(),
            };
            let selected = resolver.resolve(&pdf, "ada", &personal, None).unwrap();
            assert_eq!(selected.package_id, "so.tilo.viewer");
            assert_eq!(selected.source, SelectionSource::DeterministicFallback);
        }
        // Phase `explicit_personal_escape_never_falls_back_to_workspace_policy`.
        {
            let pdf = r("pdf");
            let apps = [
                app("so.tilo.a", "A", "pdf"),
                app("so.tilo.workspace", "Workspace", "pdf"),
            ];
            let resolver = DefaultAppResolver::new(&apps);
            let personal = PersonalDefaults::default();
            let mut ws = WorkspaceDefaults::default();
            ws.policy.insert(
                pdf.clone(),
                WorkspaceDefaultPolicy::App("so.tilo.workspace".into()),
            );
            ws.user_choices
                .entry("ada".into())
                .or_default()
                .insert(pdf.clone(), UserWorkspaceChoice::PersonalDefault);
            let selected = resolver.resolve(&pdf, "ada", &personal, Some(&ws)).unwrap();
            assert_eq!(selected.package_id, "so.tilo.a");
            assert_eq!(selected.source, SelectionSource::DeterministicFallback);
        }
    }

    fn host(desktop_id: &str, name: &str, resource: &ResourceKind) -> AppCandidate {
        AppCandidate {
            package_id: crate::mimeapps::host_app_id(desktop_id),
            display_name: name.into(),
            resources: [resource.clone()].into(),
            secondary: false,
        }
    }

    #[test]
    fn tilo_packages_outrank_host_apps_and_the_host_default_orders_host_apps() {
        let pdf = ResourceKind::mime("application/pdf").unwrap();
        let chain =
            crate::mimeapps::MimeAppsChain::new(vec![crate::mimeapps::MimeAppsList::parse(
                "[Default Applications]\napplication/pdf=gone.desktop;org.gnome.Evince.desktop;\n",
            )]);
        let hosts = [
            host("okular", "Aardvark PDF", &pdf),
            host("org.gnome.Evince", "Evince", &pdf),
        ];
        let selected = DefaultAppResolver::new(&hosts)
            .with_system_defaults(&chain)
            .resolve(&pdf, "ada", &PersonalDefaults::default(), None)
            .unwrap();
        assert_eq!(selected.package_id, "org.gnome.Evince.desktop");
        assert_eq!(selected.source, SelectionSource::SystemDefault);

        let mut with_package = hosts.to_vec();
        with_package.push(AppCandidate {
            package_id: "so.tilo.pdf-viewer".into(),
            display_name: "PDF Viewer".into(),
            resources: [pdf.clone()].into(),
            secondary: false,
        });
        let selected = DefaultAppResolver::new(&with_package)
            .with_system_defaults(&chain)
            .resolve(&pdf, "ada", &PersonalDefaults::default(), None)
            .unwrap();
        assert_eq!(selected.package_id, "so.tilo.pdf-viewer");

        let personal = PersonalDefaults {
            handlers: [(pdf.clone(), "okular.desktop".into())].into(),
        };
        let selected = DefaultAppResolver::new(&with_package)
            .with_system_defaults(&chain)
            .resolve(&pdf, "ada", &personal, None)
            .unwrap();
        assert_eq!(
            selected.package_id, "okular.desktop",
            "a personal choice of a host app wins over a package"
        );
    }

    #[test]
    fn a_secondary_claimant_is_the_default_only_when_nothing_else_claims() {
        let mp4 = r("mp4");
        let mov = r("mov");
        let editor = AppCandidate {
            resources: [mp4.clone(), mov.clone()].into(),
            secondary: true,
            ..app("so.tilo.video-editor", "Video Editor", "mp4")
        };
        let apps = [editor, app("so.tilo.video-player", "Video Player", "mp4")];
        let resolver = DefaultAppResolver::new(&apps);
        let personal = PersonalDefaults::default();
        assert_eq!(
            resolver
                .resolve(&mp4, "ada", &personal, None)
                .unwrap()
                .package_id,
            "so.tilo.video-player",
            "the player opens video ahead of the editor whose name sorts first"
        );
        assert_eq!(
            resolver
                .resolve(&mov, "ada", &personal, None)
                .unwrap()
                .package_id,
            "so.tilo.video-editor"
        );
        let chosen = PersonalDefaults {
            handlers: [(mp4.clone(), "so.tilo.video-editor".into())].into(),
        };
        assert_eq!(
            resolver
                .resolve(&mp4, "ada", &chosen, None)
                .unwrap()
                .package_id,
            "so.tilo.video-editor",
            "choosing the editor still makes it the default"
        );
    }

    #[test]
    fn scheme_handlers_use_the_x_scheme_handler_spelling() {
        let https = ResourceKind::scheme("https").unwrap();
        assert_eq!(
            https.mimeapps_type().as_deref(),
            Some("x-scheme-handler/https")
        );
        assert_eq!(
            ResourceKind::from_mimeapps_type("x-scheme-handler/HTTPS"),
            Some(https)
        );
        assert_eq!(
            ResourceKind::from_mimeapps_type("text/plain"),
            ResourceKind::mime("text/plain").ok()
        );
        assert_eq!(r("pdf").mimeapps_type(), None);
    }

    #[test]
    fn defaults_persist_atomically_and_normalize_resource_identifiers() {
        // Phase `a_settings_section_round_trips`.
        {
            let defaults = PersonalDefaults {
                handlers: [(r("pdf"), "so.tilo.viewer".into())].into(),
            };
            let serde_json::Value::Object(section) = serde_json::to_value(&defaults).unwrap()
            else {
                panic!("defaults are an object");
            };
            assert_eq!(PersonalDefaults::from_section(&section), defaults);
            assert_eq!(
                PersonalDefaults::from_section(&serde_json::Map::new()),
                PersonalDefaults::default()
            );
        }
        // Phase `resource_identifiers_are_normalized_and_active_schemes_rejected`.
        {
            assert_eq!(
                ResourceKind::mime(" Text/HTML ").unwrap().as_str(),
                "mime:text/html"
            );
            assert_eq!(
                ResourceKind::scheme("HTTPS:").unwrap().as_str(),
                "scheme:https"
            );
            for mime in [
                "text",
                "text/*",
                "text/html; charset=utf-8",
                "text/html/extra",
            ] {
                assert!(ResourceKind::mime(mime).is_err(), "accepted {mime:?}");
            }
            for scheme in ["file", "data", "javascript", "1https", "bad scheme"] {
                assert!(ResourceKind::scheme(scheme).is_err(), "accepted {scheme:?}");
            }
        }
    }
}
