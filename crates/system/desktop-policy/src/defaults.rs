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
    /// [`DefaultAppError::InvalidResource`] for an empty extension, one over 64
    /// characters, or one with characters other than letters, digits, `+`, `-`
    /// or `_`.
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
    /// [`DefaultAppError::InvalidResource`] unless `mime` is one non-wildcard
    /// type whose sides hold valid token characters.
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
    /// [`DefaultAppError::InvalidResource`] unless `scheme` is one registered
    /// name a second `:`-free remainder can follow, not `file`, `data` or
    /// `javascript`.
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

    /// The kind an id names (`ext:pdf`, `mime:text/html`, `scheme:https`),
    /// as [`Self::as_str`] writes it; `None` for anything else.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let (prefix, name) = id.split_once(':')?;
        match prefix {
            "ext" => Self::extension(name).ok(),
            "mime" => Self::mime(name).ok(),
            "scheme" => Self::scheme(name).ok(),
            _ => None,
        }
        .filter(|parsed| parsed.as_str() == id)
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
    /// Picked for these resources on purpose rather than opened by default (an
    /// editor that imports video): without an explicit choice, another app claiming
    /// the resource wins. The manifest's `[providers].secondary`.
    pub secondary: bool,
}

impl AppCandidate {
    /// A Tilo package, claiming the file extensions, MIME types and URI schemes its signed
    /// manifest declares.
    #[must_use]
    pub fn package(
        package_id: String,
        display_name: String,
        extensions: &[String],
        mime_types: &[String],
        schemes: &[String],
        secondary: bool,
    ) -> Self {
        Self {
            package_id,
            display_name,
            resources: extensions
                .iter()
                .filter_map(|extension| ResourceKind::extension(extension).ok())
                .chain(
                    mime_types
                        .iter()
                        .filter_map(|mime| ResourceKind::mime(mime).ok()),
                )
                .chain(
                    schemes
                        .iter()
                        .filter_map(|scheme| ResourceKind::scheme(scheme).ok()),
                )
                .collect(),
            secondary,
        }
    }

    /// One of the machine's own applications, by its desktop-file id, claiming the types its
    /// entry's `MimeType=` names — `x-scheme-handler/<scheme>` for a link. `None` for an entry
    /// that claims nothing: an app that opens no type is no candidate.
    #[must_use]
    pub fn host(desktop_id: &str, display_name: String, mime_types: &[String]) -> Option<Self> {
        let resources: BTreeSet<ResourceKind> = mime_types
            .iter()
            .filter_map(|mime| ResourceKind::from_mimeapps_type(mime))
            .collect();
        (!resources.is_empty()).then(|| Self {
            package_id: crate::mimeapps::host_app_id(desktop_id),
            display_name,
            resources,
            secondary: false,
        })
    }

    pub fn handles(&self, resource: &ResourceKind) -> bool {
        self.resources.contains(resource)
    }

    /// The desktop-file id of the machine's own application this is, if it is one; a Tilo
    /// package's own id else.
    #[must_use]
    pub fn host_desktop_id(&self) -> Option<&str> {
        crate::mimeapps::host_desktop_id(&self.package_id)
    }

    /// A host desktop entry rather than a Tilo package; see [`crate::mimeapps`].
    pub fn is_host(&self) -> bool {
        self.host_desktop_id().is_some()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "app")]
pub enum WorkspaceDefaultPolicy {
    #[default]
    FollowPersonal,
    App(String),
}

/// A person's choice inside one workspace. `FollowWorkspace` is the absent state:
/// it follows an admin app when set and otherwise reaches personal defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "app")]
pub enum UserWorkspaceChoice {
    #[default]
    FollowWorkspace,
    PersonalDefault,
    App(String),
}

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

    /// Consult the host's `mimeapps.list` chain when choosing among host
    /// applications that no personal or workspace choice names.
    #[must_use]
    pub fn with_system_defaults(mut self, chain: &'a crate::mimeapps::MimeAppsChain) -> Self {
        self.system_defaults = Some(chain);
        self
    }

    /// # Errors
    ///
    /// [`DefaultAppError::InvalidPackage`] if `package_id` fails
    /// [`validate_package_id`], [`DefaultAppError::NotInstalled`] if it names no
    /// candidate, or [`DefaultAppError::DoesNotHandle`] if the candidate does not
    /// claim `resource`.
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

    /// Resolve one kind of file or link, in strict precedence order. Stale/uninstalled
    /// selections are skipped rather than becoming a denial-of-open after an uninstall.
    pub fn resolve(
        &self,
        resource: &ResourceKind,
        user_id: &str,
        personal: &PersonalDefaults,
        workspace: Option<&WorkspaceDefaults>,
    ) -> Option<DefaultAppSelection> {
        self.resolve_document(std::slice::from_ref(resource), user_id, personal, workspace)
    }

    /// The app that opens one document or link, of `kinds` — the kinds it has, the narrowest
    /// first, so that an extension (`.txt`) precedes the type it names (`text/plain`): the app
    /// the workspace or the person chose for one of them; else a Tilo app that claims one; else
    /// the app this machine opens it with (`mimeapps.list`); else `None`, and the person is
    /// asked which app they want.
    ///
    /// A choice is the person's, not a claim: an app they picked for a kind opens their files of
    /// it even when its manifest claims none (a `.txt` in an app that only names Markdown is
    /// still what they asked for). Only an app this device no longer has is skipped, since no
    /// open could reach it.
    ///
    /// Callers must not reorder these layers: one file opens the same way wherever it is opened.
    pub fn resolve_document(
        &self,
        kinds: &[ResourceKind],
        user_id: &str,
        personal: &PersonalDefaults,
        workspace: Option<&WorkspaceDefaults>,
    ) -> Option<DefaultAppSelection> {
        let installed = |id: &str| self.candidates.iter().any(|app| app.package_id == id);
        let personal_id = || {
            kinds.iter().find_map(|kind| {
                personal
                    .handlers
                    .get(kind)
                    .filter(|id| installed(id.as_str()))
                    .cloned()
            })
        };

        if let Some(ws) = workspace {
            // The person's own choice in this workspace, for the narrowest kind they made one for.
            let own = kinds.iter().find_map(|kind| {
                ws.user_choices
                    .get(user_id)
                    .and_then(|m| m.get(kind))
                    .map(|choice| (kind, choice))
            });
            match own {
                Some((_, UserWorkspaceChoice::App(id))) if installed(id) => {
                    return Some(DefaultAppSelection {
                        package_id: id.clone(),
                        source: SelectionSource::UserWorkspaceOverride,
                    });
                }
                // An explicit escape from workspace policy remains an escape when the personal
                // app has since been removed: neither the policy nor the personal defaults are
                // asked, and the safe installed fallback answers.
                Some((_, UserWorkspaceChoice::PersonalDefault)) => {
                    return match personal_id() {
                        Some(package_id) => Some(DefaultAppSelection {
                            package_id,
                            source: SelectionSource::PersonalOverride,
                        }),
                        None => self.fallback(kinds),
                    };
                }
                Some((_, UserWorkspaceChoice::App(_))) => return self.fallback(kinds),
                Some((_, UserWorkspaceChoice::FollowWorkspace)) | None => {}
            }
            // The workspace's own rule, for the narrowest kind it names an installed app for.
            if let Some(id) = kinds.iter().find_map(|kind| match ws.policy.get(kind) {
                Some(WorkspaceDefaultPolicy::App(id)) if installed(id) => Some(id),
                _ => None,
            }) {
                return Some(DefaultAppSelection {
                    package_id: id.clone(),
                    source: SelectionSource::WorkspacePolicy,
                });
            }
        }
        if let Some(package_id) = personal_id() {
            return Some(DefaultAppSelection {
                package_id,
                source: SelectionSource::PersonalDefault,
            });
        }
        self.fallback(kinds)
    }

    /// Every app that claims any of `kinds`, in the order the resolver tries them: a Tilo package
    /// first, since it runs sandboxed and collaborative, one that opens the kind ahead of one
    /// only picked for it; then the machine's own applications by name.
    #[must_use]
    pub fn claimants(&self, kinds: &[ResourceKind]) -> Vec<&'a AppCandidate> {
        let mut candidates: Vec<&AppCandidate> = self
            .candidates
            .iter()
            .filter(|app| kinds.iter().any(|kind| app.handles(kind)))
            .collect();
        candidates.sort_by(|a, b| {
            a.is_host()
                .cmp(&b.is_host())
                .then_with(|| a.secondary.cmp(&b.secondary))
                .then_with(|| a.display_name.cmp(&b.display_name))
                .then_with(|| a.package_id.cmp(&b.package_id))
        });
        candidates
    }

    /// Without an explicit choice: a Tilo package that claims one of `kinds`; then the host's own
    /// `mimeapps.list` default for one of them; then any host application, deterministically by
    /// name.
    fn fallback(&self, kinds: &[ResourceKind]) -> Option<DefaultAppSelection> {
        let candidates = self.claimants(kinds);
        let first = candidates.first()?;
        if !first.is_host() {
            return Some(DefaultAppSelection {
                package_id: first.package_id.clone(),
                source: SelectionSource::DeterministicFallback,
            });
        }
        if let Some(chain) = self.system_defaults {
            for kind in kinds {
                let Some(mime) = kind.mimeapps_type() else {
                    continue;
                };
                if let Some(id) = chain.default_for(&mime, |id| {
                    candidates.iter().any(|app| app.package_id == id)
                }) {
                    return Some(DefaultAppSelection {
                        package_id: id.to_string(),
                        source: SelectionSource::SystemDefault,
                    });
                }
            }
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
    /// An error if [`DefaultAppResolver::validate_app`] rejects `package_id` for
    /// `resource`; the existing handler is untouched.
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

    /// The person's defaults, from the [`PERSONAL_DEFAULTS_SECTION`] section of
    /// their settings, which follows them to each of their devices.
    #[cfg(feature = "host-storage")]
    pub fn load() -> Self {
        Self::from_section(&settings_store::load_section(PERSONAL_DEFAULTS_SECTION))
    }

    /// Write these defaults to the person's settings.
    ///
    /// # Errors
    ///
    /// [`DefaultAppError::Persist`] when the settings cannot be written.
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
/// [`DefaultAppError::InvalidPackage`] if `id` is empty, over 255 characters,
/// starts or ends with `.`, contains `..`, or holds anything but letters,
/// digits, `.`, `-` or `_`.
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

    fn host(desktop_id: &str, name: &str, resource: &ResourceKind) -> AppCandidate {
        AppCandidate {
            package_id: crate::mimeapps::host_app_id(desktop_id),
            display_name: name.into(),
            resources: [resource.clone()].into(),
            secondary: false,
        }
    }

    fn chosen(pairs: &[(ResourceKind, &str)]) -> PersonalDefaults {
        PersonalDefaults {
            handlers: pairs
                .iter()
                .map(|(kind, id)| (kind.clone(), (*id).to_string()))
                .collect(),
        }
    }

    fn opens(
        resolver: &DefaultAppResolver<'_>,
        kinds: &[ResourceKind],
        personal: &PersonalDefaults,
        workspace: Option<&WorkspaceDefaults>,
    ) -> (String, SelectionSource) {
        let selected = resolver
            .resolve_document(kinds, "ada", personal, workspace)
            .expect("a selection");
        (selected.package_id, selected.source)
    }

    /// A person opens a PDF in someone else's workspace: the workspace's rule applies,
    /// the person may escape it, and a choice naming an app this device no longer has
    /// never becomes a denied open.
    #[test]
    fn a_document_opens_by_workspace_rule_then_person_then_first_app() {
        let pdf = r("pdf");
        let apps = [
            app("so.tilo.a", "A", "pdf"),
            app("so.tilo.b", "B", "pdf"),
            app("so.tilo.c", "C", "pdf"),
        ];
        let resolver = DefaultAppResolver::new(&apps);
        let personal = chosen(&[(pdf.clone(), "so.tilo.a")]);
        let mut ws = WorkspaceDefaults::default();
        let id = |resolver: &DefaultAppResolver<'_>,
                  personal: &PersonalDefaults,
                  ws: &WorkspaceDefaults| {
            opens(resolver, std::slice::from_ref(&pdf), personal, Some(ws)).0
        };

        assert_eq!(id(&resolver, &personal, &ws), "so.tilo.a");
        ws.policy
            .insert(pdf.clone(), WorkspaceDefaultPolicy::App("so.tilo.b".into()));
        assert_eq!(id(&resolver, &personal, &ws), "so.tilo.b");
        ws.user_choices
            .entry("ada".into())
            .or_default()
            .insert(pdf.clone(), UserWorkspaceChoice::PersonalDefault);
        assert_eq!(id(&resolver, &personal, &ws), "so.tilo.a");
        ws.user_choices
            .get_mut("ada")
            .unwrap()
            .insert(pdf.clone(), UserWorkspaceChoice::App("so.tilo.c".into()));
        assert_eq!(id(&resolver, &personal, &ws), "so.tilo.c");
        ws.policy
            .insert(pdf.clone(), WorkspaceDefaultPolicy::FollowPersonal);
        ws.user_choices
            .get_mut("ada")
            .unwrap()
            .insert(pdf.clone(), UserWorkspaceChoice::FollowWorkspace);
        assert_eq!(id(&resolver, &personal, &ws), "so.tilo.a");

        let viewer = [app("so.tilo.viewer", "Viewer", "pdf")];
        let gone = chosen(&[(pdf.clone(), "so.tilo.removed")]);
        assert_eq!(
            DefaultAppResolver::new(&viewer).resolve(&pdf, "ada", &gone, None),
            Some(DefaultAppSelection {
                package_id: "so.tilo.viewer".into(),
                source: SelectionSource::DeterministicFallback,
            }),
            "a default naming an app this device no longer has is skipped"
        );

        let mut ws = WorkspaceDefaults::default();
        ws.policy.insert(
            pdf.clone(),
            WorkspaceDefaultPolicy::App("so.tilo.workspace".into()),
        );
        ws.user_choices
            .entry("ada".into())
            .or_default()
            .insert(pdf.clone(), UserWorkspaceChoice::PersonalDefault);
        let apps = [
            app("so.tilo.a", "A", "pdf"),
            app("so.tilo.workspace", "Workspace", "pdf"),
        ];
        assert_eq!(
            opens(
                &DefaultAppResolver::new(&apps),
                std::slice::from_ref(&pdf),
                &PersonalDefaults::default(),
                Some(&ws)
            ),
            ("so.tilo.a".into(), SelectionSource::DeterministicFallback),
            "an escape from workspace policy stays an escape when the personal app is gone"
        );
    }

    /// A document has the kinds it has — its extension and the type it names — and one
    /// file opens the same way whichever of them an app claims.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one flow through every layer the resolver composes, with each former test's assertions kept"
    )]
    fn a_document_opens_by_any_of_its_kinds_and_a_package_beats_a_host_app() {
        let txt = r("txt");
        let plain = ResourceKind::mime("text/plain").unwrap();
        let kinds = [txt.clone(), plain.clone()];
        let notes = AppCandidate::package(
            "so.tilo.notes".into(),
            "Notes".into(),
            &["txt".into()],
            &[],
            &[],
            false,
        );
        let editor = AppCandidate::host(
            "org.gnome.TextEditor",
            "Text Editor".into(),
            &["text/plain".into()],
        )
        .expect("an entry that claims a type");
        let chain =
            crate::mimeapps::MimeAppsChain::new(vec![crate::mimeapps::MimeAppsList::parse(
                "[Default Applications]\ntext/plain=org.gnome.TextEditor.desktop;\n",
            )]);
        let personal = PersonalDefaults::default();

        let first = [editor.clone(), notes];
        assert_eq!(
            opens(
                &DefaultAppResolver::new(&first).with_system_defaults(&chain),
                &kinds,
                &personal,
                None
            )
            .0,
            "so.tilo.notes",
            "a Tilo app claiming any of the document's kinds opens it, before the machine's own"
        );
        assert_eq!(
            opens(
                &DefaultAppResolver::new(std::slice::from_ref(&editor))
                    .with_system_defaults(&chain),
                &kinds,
                &personal,
                None
            ),
            (
                "org.gnome.TextEditor.desktop".into(),
                SelectionSource::SystemDefault
            ),
            "the machine's own default decides between its apps"
        );
        assert_eq!(
            DefaultAppResolver::new(&[]).resolve_document(&kinds, "ada", &personal, None),
            None,
            "nothing claims it: the person is asked"
        );

        // Notes names Markdown only, and the person still picked it for `.txt` files.
        let notes = AppCandidate::package(
            "so.tilo.notes".into(),
            "Notes".into(),
            &["md".into()],
            &[],
            &[],
            false,
        );
        let apps = [editor, notes];
        let resolver = DefaultAppResolver::new(&apps);
        assert_eq!(
            opens(
                &resolver,
                std::slice::from_ref(&txt),
                &chosen(&[(txt.clone(), "so.tilo.notes")]),
                None
            ),
            ("so.tilo.notes".into(), SelectionSource::PersonalDefault),
            "a choice is the person's, not a claim"
        );
        let gone = chosen(&[(txt.clone(), "so.tilo.gone")]);
        assert_eq!(
            opens(&resolver, &kinds, &gone, None).0,
            "org.gnome.TextEditor.desktop",
            "an app this device does not have is no answer: the open would reach nothing"
        );

        // A workspace rule for the type and a person's choice for the extension, on one
        // document.
        let notes = AppCandidate::package(
            "so.tilo.notes".into(),
            "Notes".into(),
            &[],
            &["text/plain".into()],
            &[],
            false,
        );
        let writer = app("so.tilo.word-processor", "Word Processor", "txt");
        let apps = [notes, writer];
        let resolver = DefaultAppResolver::new(&apps);
        let mut ws = WorkspaceDefaults::default();
        ws.policy
            .insert(plain, WorkspaceDefaultPolicy::App("so.tilo.notes".into()));
        assert_eq!(
            opens(&resolver, &kinds, &personal, Some(&ws)),
            ("so.tilo.notes".into(), SelectionSource::WorkspacePolicy),
            "the workspace's choice names the type, and the document is of that type"
        );
        ws.user_choices.entry("ada".into()).or_default().insert(
            txt.clone(),
            UserWorkspaceChoice::App("so.tilo.word-processor".into()),
        );
        assert_eq!(
            opens(&resolver, &kinds, &personal, Some(&ws)),
            (
                "so.tilo.word-processor".into(),
                SelectionSource::UserWorkspaceOverride
            ),
            "the person's own choice, made for the narrower kind, is theirs"
        );
        assert_eq!(
            opens(
                &resolver,
                &kinds,
                &chosen(&[(txt, "so.tilo.word-processor")]),
                None
            )
            .0,
            "so.tilo.word-processor",
            "the person's own default opens it outside any workspace"
        );

        // PDF: the machine's own apps, and the host's own default between them.
        let pdf = ResourceKind::mime("application/pdf").unwrap();
        let chain =
            crate::mimeapps::MimeAppsChain::new(vec![crate::mimeapps::MimeAppsList::parse(
                "[Default Applications]\napplication/pdf=gone.desktop;org.gnome.Evince.desktop;\n",
            )]);
        let hosts = [
            host("okular", "Aardvark PDF", &pdf),
            host("org.gnome.Evince", "Evince", &pdf),
        ];
        assert_eq!(
            opens(
                &DefaultAppResolver::new(&hosts).with_system_defaults(&chain),
                std::slice::from_ref(&pdf),
                &personal,
                None
            ),
            (
                "org.gnome.Evince.desktop".into(),
                SelectionSource::SystemDefault
            )
        );
        let mut with_package = hosts.to_vec();
        with_package.push(AppCandidate {
            package_id: "so.tilo.pdf-viewer".into(),
            display_name: "PDF Viewer".into(),
            resources: [pdf.clone()].into(),
            secondary: false,
        });
        assert_eq!(
            opens(
                &DefaultAppResolver::new(&with_package).with_system_defaults(&chain),
                std::slice::from_ref(&pdf),
                &personal,
                None
            )
            .0,
            "so.tilo.pdf-viewer"
        );
        assert_eq!(
            opens(
                &DefaultAppResolver::new(&with_package).with_system_defaults(&chain),
                std::slice::from_ref(&pdf),
                &chosen(&[(pdf.clone(), "okular.desktop")]),
                None
            )
            .0,
            "okular.desktop",
            "a personal choice of a host app wins over a package"
        );
    }

    /// An editor that imports video claims it on purpose rather than opening it by
    /// default: another app claiming the resource wins until the person picks it.
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
            opens(&resolver, std::slice::from_ref(&mp4), &personal, None).0,
            "so.tilo.video-player",
            "the player opens video ahead of the editor whose name sorts first"
        );
        assert_eq!(
            opens(&resolver, std::slice::from_ref(&mov), &personal, None).0,
            "so.tilo.video-editor"
        );
        assert_eq!(
            opens(
                &resolver,
                std::slice::from_ref(&mp4),
                &chosen(&[(mp4.clone(), "so.tilo.video-editor")]),
                None
            )
            .0,
            "so.tilo.video-editor",
            "choosing the editor still makes it the default"
        );
    }

    /// Identifiers are normalized before they are compared, so a choice made by any
    /// spelling reaches the file; and a default the person saved comes back on their
    /// next device.
    #[test]
    fn a_default_persists_and_resource_identifiers_are_normalized() {
        let defaults = PersonalDefaults {
            handlers: [(r("pdf"), "so.tilo.viewer".into())].into(),
        };
        let serde_json::Value::Object(section) = serde_json::to_value(&defaults).unwrap() else {
            panic!("defaults are an object");
        };
        assert_eq!(PersonalDefaults::from_section(&section), defaults);
        assert_eq!(
            PersonalDefaults::from_section(&serde_json::Map::new()),
            PersonalDefaults::default()
        );
        assert!(validate_package_id(&crate::mimeapps::host_app_id("kde-okular")).is_ok());

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

        let https = ResourceKind::scheme("https").unwrap();
        assert_eq!(
            https.mimeapps_type().as_deref(),
            Some("x-scheme-handler/https"),
            "the spelling host tools read"
        );
        assert_eq!(
            ResourceKind::from_mimeapps_type("x-scheme-handler/HTTPS"),
            Some(https)
        );
        assert_eq!(r("pdf").mimeapps_type(), None);
    }
}
