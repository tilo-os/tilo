use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
pub use tilo_mime::TypeCategory;

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

    /// The category of the taxonomy ([`tilo_mime::TypeCategory`]) the
    /// resource belongs to; [`TypeCategory::Other`] for anything unknown.
    pub fn category(&self) -> TypeCategory {
        if let Some(ext) = self.extension_name() {
            TypeCategory::of_extension(ext)
        } else if let Some(mime) = self.mime_name() {
            TypeCategory::of_mime(mime)
        } else if let Some(scheme) = self.scheme_name() {
            TypeCategory::of_scheme(scheme)
        } else {
            TypeCategory::Other
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
    /// The app one type opens with, overriding its category's.
    #[serde(default)]
    pub handlers: BTreeMap<ResourceKind, String>,
    /// The app every type of a category opens with unless the type has a
    /// handler of its own, by the category's slug
    /// ([`TypeCategory::slug`]). Keyed by string so that a category a
    /// newer version adds survives this one's writes.
    #[serde(default)]
    pub categories: BTreeMap<String, String>,
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
    /// The person's choice for the resource's whole category
    /// ([`PersonalDefaults::categories`]): weaker than a choice for the
    /// type itself, whichever of a file's kinds names it.
    PersonalCategory,
    /// The host's `mimeapps.list` chain named this host application.
    SystemDefault,
    DeterministicFallback,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
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
        let category_id = || {
            personal
                .category_app(resource.category())
                .filter(|id| valid(id))
        };

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
                    if let Some(id) = category_id() {
                        return Some(DefaultAppSelection {
                            package_id: id.to_owned(),
                            source: SelectionSource::PersonalCategory,
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
        if let Some(id) = category_id() {
            return Some(DefaultAppSelection {
                package_id: id.to_owned(),
                source: SelectionSource::PersonalCategory,
            });
        }
        self.fallback(resource)
    }

    /// The app a file of type `file_type` opens with for this person, as
    /// the launcher ranks a file's MIME type and extension: a choice for
    /// the MIME type, then one for the extension, then the extension's
    /// category's choice, then the MIME type's, then the fallback.
    pub fn resolve_type(
        &self,
        file_type: &FileType,
        personal: &PersonalDefaults,
    ) -> Option<DefaultAppSelection> {
        let own = |kind: &ResourceKind| {
            personal
                .handlers
                .get(kind)
                .filter(|id| self.validate_app(kind, id).is_ok())
                .map(|id| DefaultAppSelection {
                    package_id: id.clone(),
                    source: SelectionSource::PersonalDefault,
                })
        };
        file_type
            .launch_mime
            .as_ref()
            .and_then(own)
            .or_else(|| file_type.primary().and_then(own))
            .or_else(|| self.inherited(file_type, personal))
    }

    /// The app `file_type` opens with when it has no choice of its own:
    /// its category's choice, else the fallback. What a reset returns it
    /// to.
    pub fn inherited(
        &self,
        file_type: &FileType,
        personal: &PersonalDefaults,
    ) -> Option<DefaultAppSelection> {
        let category = |kind: &ResourceKind| {
            personal
                .category_app(kind.category())
                .filter(|id| self.validate_app(kind, id).is_ok())
                .map(|id| DefaultAppSelection {
                    package_id: id.to_owned(),
                    source: SelectionSource::PersonalCategory,
                })
        };
        let primary = file_type.primary();
        primary
            .and_then(category)
            .or_else(|| file_type.launch_mime.as_ref().and_then(category))
            .or_else(|| primary.and_then(|kind| self.fallback(kind)))
            .or_else(|| {
                file_type
                    .launch_mime
                    .as_ref()
                    .and_then(|m| self.fallback(m))
            })
    }

    /// Whether `package_id` is installed and claims at least one resource
    /// of `category`, so it may be the whole category's choice.
    ///
    /// # Errors
    ///
    /// [`DefaultAppError::InvalidPackage`] or
    /// [`DefaultAppError::NotInstalled`] as for [`Self::validate_app`], or
    /// [`DefaultAppError::DoesNotHandle`] if it claims nothing of the
    /// category.
    pub fn validate_category_app(
        &self,
        category: TypeCategory,
        package_id: &str,
    ) -> Result<(), DefaultAppError> {
        validate_package_id(package_id)?;
        let Some(app) = self.candidates.iter().find(|a| a.package_id == package_id) else {
            return Err(DefaultAppError::NotInstalled);
        };
        if app.resources.iter().any(|kind| kind.category() == category) {
            Ok(())
        } else {
            Err(DefaultAppError::DoesNotHandle {
                package_id: package_id.to_string(),
                resource: category.slug().to_string(),
            })
        }
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

/// A type of file or link as a person chooses an app for it: an extension
/// with the MIME type its files carry and the other extensions of that MIME
/// type (`.jpeg` and `.jpg`), or a link's scheme.
///
/// Files are opened by both their MIME type and their extension, so a
/// choice for the type covers both. The MIME type is the type's own when it
/// is in the same category and no file of another category carries it
/// (`application/pdf` for `.pdf`, never `text/plain` for `.py`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType {
    /// The extensions (or the one scheme or MIME type) the type is, the
    /// one it was named by first.
    pub kinds: Vec<ResourceKind>,
    /// The MIME type a choice for the type also covers.
    pub mime: Option<ResourceKind>,
    /// The MIME type the launcher reads for a file of this type, shared
    /// or not: a choice for it outranks the extension's.
    pub launch_mime: Option<ResourceKind>,
}

impl FileType {
    /// The type `kind` names: an extension with its MIME type and that
    /// type's other extensions, or a MIME type or scheme alone.
    pub fn of(kind: ResourceKind) -> Self {
        let Some(ext) = kind.extension_name().map(str::to_owned) else {
            return Self {
                kinds: vec![kind],
                mime: None,
                launch_mime: None,
            };
        };
        let category = TypeCategory::of_extension(&ext);
        let launch_mime = tilo_mime::from_ext(&ext).and_then(|mime| ResourceKind::mime(mime).ok());
        let mime = launch_mime.as_ref().and_then(|mime| {
            let name = mime.mime_name()?;
            let own = TypeCategory::of_mime(name) == category
                && tilo_mime::extensions_of(name)
                    .all(|other| TypeCategory::of_extension(other) == category);
            own.then(|| mime.clone())
        });
        let siblings: Vec<ResourceKind> = mime
            .as_ref()
            .and_then(ResourceKind::mime_name)
            .map(|name| {
                tilo_mime::extensions_of(name)
                    .filter(|other| *other != ext.as_str())
                    .filter_map(|other| ResourceKind::extension(other).ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut kinds = vec![kind];
        kinds.extend(siblings);
        Self {
            kinds,
            mime,
            launch_mime,
        }
    }

    /// The extension, scheme or MIME type the type was named by.
    pub fn primary(&self) -> Option<&ResourceKind> {
        self.kinds.first()
    }

    /// What tells the type apart from every other: its own MIME type,
    /// else the extension or scheme it is. Two extensions of one type
    /// (`.jpeg`, `.jpg`) have the same key.
    pub fn key(&self) -> Option<&ResourceKind> {
        self.mime.as_ref().or_else(|| self.primary())
    }

    /// The category the type belongs to.
    pub fn category(&self) -> TypeCategory {
        self.primary()
            .map_or(TypeCategory::Other, ResourceKind::category)
    }

    /// Every resource a choice for the type covers.
    fn covered(&self) -> impl Iterator<Item = &ResourceKind> {
        self.kinds.iter().chain(self.mime.as_ref())
    }

    /// Whether the person chose an app for this type itself, rather than
    /// following its category.
    pub fn is_overridden(&self, personal: &PersonalDefaults) -> bool {
        self.covered()
            .any(|kind| personal.handlers.contains_key(kind))
    }

    /// Whether `app` opens any resource of the type.
    pub fn claimed_by(&self, app: &AppCandidate) -> bool {
        self.covered().any(|kind| app.handles(kind))
    }

    /// The handler of each of the type's resources once `package_id` is
    /// chosen for it: the app for each it claims, none for the others, so
    /// an older choice for one of them does not outrank the new one.
    ///
    /// # Errors
    ///
    /// Returns [`DefaultAppResolver::validate_app`]'s error for the type
    /// when the app claims none of its resources.
    pub fn choose(
        &self,
        package_id: &str,
        resolver: &DefaultAppResolver<'_>,
    ) -> Result<Vec<(ResourceKind, Option<String>)>, DefaultAppError> {
        let mut first_error = None;
        let mut claimed_any = false;
        let mut out = Vec::new();
        for kind in self.covered() {
            match resolver.validate_app(kind, package_id) {
                Ok(()) => {
                    claimed_any = true;
                    out.push((kind.clone(), Some(package_id.to_owned())));
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    out.push((kind.clone(), None));
                }
            }
        }
        if claimed_any {
            Ok(out)
        } else {
            Err(first_error.unwrap_or(DefaultAppError::InvalidResource))
        }
    }

    /// The handlers a reset leaves: none of the type's own, so it follows
    /// its category again.
    pub fn reset(&self) -> Vec<(ResourceKind, Option<String>)> {
        self.covered().map(|kind| (kind.clone(), None)).collect()
    }
}

impl PersonalDefaults {
    /// The app the person chose for every type of `category`, if any.
    pub fn category_app(&self, category: TypeCategory) -> Option<&str> {
        self.categories.get(category.slug()).map(String::as_str)
    }

    /// Choose `package_id` for every type of `category` that has no choice
    /// of its own; the types' own choices stay.
    ///
    /// # Errors
    ///
    /// Returns [`DefaultAppResolver::validate_category_app`]'s error; the
    /// existing choice is left untouched.
    pub fn set_category(
        &mut self,
        category: TypeCategory,
        package_id: String,
        resolver: &DefaultAppResolver<'_>,
    ) -> Result<(), DefaultAppError> {
        resolver.validate_category_app(category, &package_id)?;
        self.categories
            .insert(category.slug().to_owned(), package_id);
        Ok(())
    }

    /// Choose `package_id` for `file_type` itself, over its category's.
    ///
    /// # Errors
    ///
    /// As [`FileType::choose`]; nothing changes then.
    pub fn set_type(
        &mut self,
        file_type: &FileType,
        package_id: &str,
        resolver: &DefaultAppResolver<'_>,
    ) -> Result<(), DefaultAppError> {
        let handlers = file_type.choose(package_id, resolver)?;
        self.apply(handlers);
        Ok(())
    }

    /// Drop `file_type`'s own choice, so it follows its category again.
    pub fn reset_type(&mut self, file_type: &FileType) {
        self.apply(file_type.reset());
    }

    fn apply(&mut self, handlers: Vec<(ResourceKind, Option<String>)>) {
        for (kind, app) in handlers {
            match app {
                Some(app) => {
                    self.handlers.insert(kind, app);
                }
                None => {
                    self.handlers.remove(&kind);
                }
            }
        }
    }

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
                ..PersonalDefaults::default()
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
                ..PersonalDefaults::default()
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

    fn claims(id: &str, name: &str, kinds: &[ResourceKind]) -> AppCandidate {
        AppCandidate {
            package_id: id.into(),
            display_name: name.into(),
            resources: kinds.iter().cloned().collect(),
            secondary: false,
        }
    }

    /// Two code editors and a notes app: `.rs` and `.toml` are Code, `.md`
    /// is Documents.
    fn code_apps() -> [AppCandidate; 3] {
        [
            claims(
                "so.tilo.code-editor",
                "Code Editor",
                &[r("rs"), r("toml"), r("py")],
            ),
            claims("org.example.zed", "Zed", &[r("rs"), r("toml"), r("py")]),
            claims("so.tilo.notes", "Notes", &[r("md")]),
        ]
    }

    fn opens(resolver: &DefaultAppResolver<'_>, personal: &PersonalDefaults, ext: &str) -> String {
        resolver
            .resolve_type(&FileType::of(r(ext)), personal)
            .map(|selected| selected.package_id)
            .unwrap_or_default()
    }

    // USER REQUIREMENT (2026-10-21): "a category-level change applies to non-overridden types" ("Changing the category's app applies to all its types unless a type is overridden")
    #[test]
    fn a_category_level_change_applies_to_non_overridden_types() {
        let apps = code_apps();
        let resolver = DefaultAppResolver::new(&apps);
        let mut personal = PersonalDefaults::default();
        assert_eq!(opens(&resolver, &personal, "rs"), "so.tilo.code-editor");
        personal
            .set_category(TypeCategory::Code, "org.example.zed".into(), &resolver)
            .unwrap();
        for ext in ["rs", "toml", "py"] {
            assert_eq!(opens(&resolver, &personal, ext), "org.example.zed", "{ext}");
            assert_eq!(
                resolver
                    .resolve(&r(ext), "", &personal, None)
                    .unwrap()
                    .source,
                SelectionSource::PersonalCategory,
                "the launcher reads the category's choice for .{ext} too"
            );
        }
        assert_eq!(
            opens(&resolver, &personal, "md"),
            "so.tilo.notes",
            "another category is untouched"
        );
        assert!(
            personal
                .set_category(TypeCategory::Code, "so.tilo.notes".into(), &resolver)
                .is_err(),
            "an app that opens nothing of the category cannot be its choice"
        );
    }

    // USER REQUIREMENT (2026-10-21): "overrides survive a category change"
    #[test]
    fn overrides_survive_a_category_change() {
        let apps = code_apps();
        let resolver = DefaultAppResolver::new(&apps);
        let mut personal = PersonalDefaults::default();
        let toml = FileType::of(r("toml"));
        personal
            .set_type(&toml, "so.tilo.code-editor", &resolver)
            .unwrap();
        assert!(toml.is_overridden(&personal));
        personal
            .set_category(TypeCategory::Code, "org.example.zed".into(), &resolver)
            .unwrap();
        assert_eq!(opens(&resolver, &personal, "rs"), "org.example.zed");
        assert_eq!(
            opens(&resolver, &personal, "toml"),
            "so.tilo.code-editor",
            "the override survives the category change"
        );
        assert!(toml.is_overridden(&personal));
        personal.reset_type(&toml);
        assert!(!toml.is_overridden(&personal));
        assert_eq!(
            opens(&resolver, &personal, "toml"),
            "org.example.zed",
            "a reset type follows its category again"
        );
    }

    #[test]
    fn a_type_covers_its_own_mime_type_but_never_a_shared_one() {
        let pdf = FileType::of(r("pdf"));
        assert_eq!(pdf.mime, ResourceKind::mime("application/pdf").ok());
        assert_eq!(pdf.category(), TypeCategory::Documents);
        // `.py` files carry `text/plain`, as `.txt` files do: choosing an
        // app for Python must not choose it for text.
        let py = FileType::of(r("py"));
        assert_eq!(py.mime, None);
        assert_eq!(py.launch_mime, ResourceKind::mime("text/plain").ok());
        assert_eq!(py.category(), TypeCategory::Code);
        // Two extensions of one type are one type.
        let jpeg = FileType::of(r("jpeg"));
        assert!(jpeg.kinds.contains(&r("jpg")));
        assert_eq!(jpeg.primary(), Some(&r("jpeg")));
        assert_eq!(jpeg.key(), FileType::of(r("jpg")).key());
        assert_ne!(py.key(), FileType::of(r("txt")).key());
        let https = FileType::of(ResourceKind::scheme("https").unwrap());
        assert_eq!(https.category(), TypeCategory::Web);
        assert_eq!(https.mime, None);

        // A choice for the type takes over an older choice for its MIME
        // type, which the launcher would otherwise rank first.
        let pdf_mime = ResourceKind::mime("application/pdf").unwrap();
        let apps = [
            claims(
                "so.tilo.pdf-viewer",
                "PDF Viewer",
                &[r("pdf"), pdf_mime.clone()],
            ),
            claims("org.example.reader", "Reader", &[r("pdf")]),
        ];
        let resolver = DefaultAppResolver::new(&apps);
        let mut personal = PersonalDefaults {
            handlers: [(pdf_mime.clone(), "so.tilo.pdf-viewer".into())].into(),
            ..PersonalDefaults::default()
        };
        assert!(pdf.is_overridden(&personal), "a choice made by MIME type");
        personal
            .set_type(&pdf, "org.example.reader", &resolver)
            .unwrap();
        assert_eq!(opens(&resolver, &personal, "pdf"), "org.example.reader");
        assert!(!personal.handlers.contains_key(&pdf_mime));
        assert!(
            personal
                .set_type(&pdf, "so.tilo.code-editor", &resolver)
                .is_err()
        );
    }

    #[test]
    fn the_categories_section_round_trips_and_keeps_unknown_categories() {
        let mut section = serde_json::Map::new();
        section.insert(
            "categories".into(),
            serde_json::json!({ "code": "org.example.zed", "holograms": "org.example.future" }),
        );
        let defaults = PersonalDefaults::from_section(&section);
        assert_eq!(
            defaults.category_app(TypeCategory::Code),
            Some("org.example.zed")
        );
        assert_eq!(
            defaults.categories.get("holograms").map(String::as_str),
            Some("org.example.future")
        );
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
            ..PersonalDefaults::default()
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
            ..PersonalDefaults::default()
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
                ..PersonalDefaults::default()
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
