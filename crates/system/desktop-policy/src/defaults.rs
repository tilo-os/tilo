use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
}

impl AppCandidate {
    pub fn handles(&self, resource: &ResourceKind) -> bool {
        self.resources.contains(resource)
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
}

impl<'a> DefaultAppResolver<'a> {
    pub fn new(candidates: &'a [AppCandidate]) -> Self {
        Self { candidates }
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

    fn fallback(&self, resource: &ResourceKind) -> Option<DefaultAppSelection> {
        let mut candidates: Vec<&AppCandidate> = self
            .candidates
            .iter()
            .filter(|app| app.handles(resource))
            .collect();
        candidates.sort_by(|a, b| {
            a.display_name
                .cmp(&b.display_name)
                .then_with(|| a.package_id.cmp(&b.package_id))
        });
        candidates.first().map(|app| DefaultAppSelection {
            package_id: app.package_id.clone(),
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

    pub fn load() -> Self {
        let Some(path) = personal_path() else {
            return Self::default();
        };
        Self::load_at(&path)
    }

    pub fn load_at(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns [`DefaultAppError::Persist`] if no config directory is
    /// available, or [`PersonalDefaults::save_at`] fails.
    pub fn save(&self) -> Result<(), DefaultAppError> {
        let path = personal_path()
            .ok_or_else(|| DefaultAppError::Persist("no config directory".into()))?;
        self.save_at(&path)
    }

    /// # Errors
    ///
    /// Returns [`DefaultAppError::Persist`] if `self` can't be serialised
    /// or written to `path`.
    pub fn save_at(&self, path: &Path) -> Result<(), DefaultAppError> {
        let mut body =
            serde_json::to_vec_pretty(self).map_err(|e| DefaultAppError::Persist(e.to_string()))?;
        body.push(b'\n');
        atomic_file::write_creating_parent(path, &body)
            .map_err(|e| DefaultAppError::Persist(e.to_string()))
    }
}

fn personal_path() -> Option<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|p| p.join(".config")))?;
    Some(root.join("tilo/default-apps.json"))
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
        }
    }

    #[test]
    fn exact_precedence_and_escape_hatches() {
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

    #[test]
    fn invalid_or_uninstalled_selection_falls_through_safely() {
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

    #[test]
    fn explicit_personal_escape_never_falls_back_to_workspace_policy() {
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

    #[test]
    fn persistence_round_trip_is_atomic_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/default-apps.json");
        let defaults = PersonalDefaults {
            handlers: [(r("pdf"), "so.tilo.viewer".into())].into(),
        };
        defaults.save_at(&path).unwrap();
        assert_eq!(PersonalDefaults::load_at(&path), defaults);
    }

    #[test]
    fn resource_identifiers_are_normalized_and_active_schemes_rejected() {
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
