//! Explicit source/provider decisions, independent of bookmarks/history and files.
#[cfg(all(test, target_os = "linux"))]
#[path = "login_tests.rs"]
mod login_tests;
mod logins;
use crate::setup::{account_transfer::Candidate, service_sessions::PreparedSessions};
use imager_backend::browser_selection::Service;
use imager_backend::service_selection::{
    self as service_profiles, CookieProfile, Material, Presence,
};
use std::{io, path::Path, sync::Arc};
use ui::{
    tokens::{Rhythm, TextRole},
    widget::{Checkbox, Column, Element, Text},
};

#[derive(Clone, Default)]
pub(in crate::setup) struct SelectedSources {
    sources: Vec<(Arc<CookieProfile>, Service)>,
}
impl SelectedSources {
    pub(in crate::setup) fn capture(
        &self,
        identity: [u8; 32],
        binding: String,
    ) -> io::Result<PreparedSessions> {
        service_profiles::capture_selected(identity, binding, &self.sources)
    }
    pub(super) fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}
#[derive(Default)]
pub(super) struct Selection {
    sources: Vec<Arc<CookieProfile>>,
    included: Vec<(String, Service)>,
    detected: Vec<(String, Service, Presence)>,
    unavailable: bool,
    decided: Vec<Service>,
    baseline: Option<(Vec<(String, Service)>, Vec<Service>)>,
    collapsed: std::collections::BTreeSet<String>,
}
impl Selection {
    pub(super) fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Self> {
        Self::from_discovery(service_profiles::discover(home, candidates)?)
    }
    fn from_discovery(mut discovered: service_profiles::Discovery) -> io::Result<Self> {
        let mut sources = std::mem::take(&mut discovered.sources);
        let mut unavailable = discovered.unavailable;
        // Stable source IDs, not directory iteration order, select the default.
        sources.sort_by(|left, right| left.id.cmp(&right.id));
        let mut detected = Vec::new();
        let mut included = Vec::new();
        for source in &sources {
            for service in [Service::Slack, Service::Discord] {
                discovered.validate_home()?;
                let observed = source.presence(service);
                discovered.validate_home()?;
                let presence = if let Ok(presence) = observed {
                    presence
                } else {
                    unavailable = true;
                    continue;
                };
                if presence == Presence::Unavailable {
                    continue;
                }
                detected.push((source.id.clone(), service, presence));
                if presence == Presence::Readable
                    && !included.iter().any(|(id, prior)| {
                        *prior == service
                            && sources
                                .iter()
                                .find(|source| &source.id == id)
                                .is_some_and(|prior| prior.material == source.material)
                    })
                {
                    included.push((source.id.clone(), service));
                }
            }
        }
        discovered.validate_home()?;
        Ok(Self {
            sources,
            included,
            detected,
            unavailable,
            decided: Vec::new(),
            baseline: None,
            collapsed: std::collections::BTreeSet::new(),
        })
    }
    pub(super) fn unavailable(&self) -> bool {
        self.unavailable
    }
    pub(super) fn available(&self) -> bool {
        !self.detected.is_empty()
    }
    /// Describe selected material, never authenticated accounts or working logins.
    pub(super) fn overview(&self, accounts_enabled: bool) -> String {
        if !accounts_enabled {
            return super::t("installer-service-sessions-accounts-excluded");
        }
        let label = |service| {
            super::t(match service {
                Service::Slack => "installer-service-slack-label",
                Service::Discord => "installer-service-discord-label",
            })
        };
        let selected = [Service::Slack, Service::Discord]
            .into_iter()
            .filter(|service| self.included.iter().any(|(_, kind)| kind == service))
            .map(label)
            .collect::<Vec<_>>()
            .join(", ");
        let needs_access = [Service::Slack, Service::Discord]
            .into_iter()
            .filter(|service| {
                self.included.iter().any(|(id, kind)| {
                    kind == service
                        && self
                            .detected
                            .iter()
                            .any(|(source_id, detected_service, presence)| {
                                source_id == id
                                    && detected_service == service
                                    && *presence == Presence::NeedsAccess
                            })
                })
            })
            .map(label)
            .collect::<Vec<_>>()
            .join(", ");
        let mut summary = if selected.is_empty() {
            super::t("installer-service-sessions-none-selected")
        } else {
            super::t_args(
                "installer-service-sessions-overview",
                &[("services", selected.into())],
            )
        };
        if !needs_access.is_empty() {
            summary.push(' ');
            summary.push_str(&super::t_args(
                "installer-service-sessions-selected-needs-access",
                &[("services", needs_access.into())],
            ));
        }
        // Initial encrypted sources need attention. An explicit service choice,
        // including exclusion, must not become a repeated access prompt.
        let found = [Service::Slack, Service::Discord]
            .into_iter()
            .filter(|service| {
                !self.decided.contains(service)
                    && !self.included.iter().any(|(_, kind)| kind == service)
                    && self.detected.iter().any(|(_, kind, presence)| {
                        kind == service && *presence == Presence::NeedsAccess
                    })
            })
            .collect::<Vec<_>>();
        if !found.is_empty() {
            summary.push(' ');
            summary.push_str(&super::t_args(
                "installer-service-sessions-found-attention",
                &[(
                    "services",
                    found
                        .iter()
                        .copied()
                        .map(label)
                        .collect::<Vec<_>>()
                        .join(", ")
                        .into(),
                )],
            ));
        }
        summary
    }
    pub(super) fn set(
        &mut self,
        id: &str,
        service: Service,
        included: bool,
    ) -> Result<bool, &'static str> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id == id)
            .ok_or("installer-service-source-changed")?;
        source
            .validate()
            .map_err(|_| "installer-service-source-changed")?;
        if !self
            .detected
            .iter()
            .any(|(source_id, kind, _)| source_id == id && *kind == service)
        {
            return Err("installer-service-source-changed");
        }
        if included
            && self.detected.iter().any(|(source_id, kind, presence)| {
                source_id == id && *kind == service && *presence == Presence::NeedsAccess
            })
            && !source.supports_encrypted_cookie_capture()
        {
            return Err("installer-service-reconnect-required");
        }
        let material = source.material;
        let decision_changed = !self.decided.contains(&service);
        if decision_changed {
            self.decided.push(service);
        }
        if self
            .included
            .iter()
            .any(|(prior, kind)| prior == id && *kind == service)
            == included
        {
            return Ok(decision_changed);
        }
        let previous = self.included.clone();
        self.included.retain(|(prior, kind)| {
            if included {
                *kind != service
                    || self
                        .sources
                        .iter()
                        .find(|source| &source.id == prior)
                        .is_some_and(|source| source.material != material)
            } else {
                prior != id || *kind != service
            }
        });
        if included {
            self.included.push((id.into(), service));
        }
        Ok(previous != self.included || decision_changed)
    }
    pub(super) fn begin_edit(&mut self) {
        self.baseline = Some((self.included.clone(), self.decided.clone()));
    }
    pub(super) fn cancel_edit(&mut self) {
        if let Some((included, decided)) = self.baseline.take() {
            self.included = included;
            self.decided = decided;
        }
    }
    pub(super) fn commit_edit(&mut self) -> bool {
        self.baseline
            .take()
            .is_some_and(|(included, decided)| included != self.included || decided != self.decided)
    }
    pub(super) fn report(&self) -> serde_json::Value {
        serde_json::json!({"sources":self.detected.iter().filter_map(|(id, service, presence)| {
            let source = self.sources.iter().find(|source| &source.id == id)?;
            Some(serde_json::json!({"id":id,"label":source.label,"application":source.application,
                "native_application":source.native_application,"service":service,
                "needs_access":*presence == Presence::NeedsAccess,
                "selectable":*presence != Presence::NeedsAccess || source.supports_encrypted_cookie_capture()}))
        }).collect::<Vec<_>>(),"selected":self.included})
    }
    pub(super) fn clear(&mut self) {
        self.included.clear();
        self.decided = vec![Service::Slack, Service::Discord];
    }
    pub(super) fn selected_for_accounts(&self, enabled: bool) -> Option<SelectedSources> {
        enabled
            .then(|| self.selected())
            .filter(|selected| !selected.is_empty())
    }
    pub(super) fn selected(&self) -> SelectedSources {
        SelectedSources {
            sources: self
                .included
                .iter()
                .filter_map(|(id, service)| {
                    self.sources
                        .iter()
                        .find(|source| source.id == *id)
                        .map(|source| (Arc::clone(source), *service))
                })
                .collect(),
        }
    }
    /// Public labels for exactly the explicitly selected source/provider pairs.
    pub(super) fn review_labels(&self, accounts_enabled: bool) -> Vec<String> {
        if !accounts_enabled {
            return Vec::new();
        }
        self.included
            .iter()
            .filter_map(|(id, service)| {
                let source = self.sources.iter().find(|source| &source.id == id)?;
                let label = super::review_source_label(&source.label);
                Some(super::t_args(
                    match service {
                        Service::Slack => "installer-service-source-slack",
                        Service::Discord if source.material == Material::DiscordOrigin => {
                            "installer-service-source-discord-origin"
                        }
                        Service::Discord => "installer-service-source-discord",
                    },
                    &[("source", label.into())],
                ))
            })
            .collect()
    }
    pub(super) fn view(
        &self,
        accounts_enabled: bool,
        bindings: impl Fn(String, Service, bool) + 'static + Clone,
    ) -> Element {
        if !accounts_enabled {
            return Box::new(Text::new(&super::t(
                "installer-service-sessions-accounts-excluded",
            )));
        }
        let mut body = Column::new()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(
                Text::new(&super::t("installer-service-sessions-title"))
                    .with_role(TextRole::Subheading),
            ))
            .push(Box::new(Text::new(&super::t(
                "installer-service-sessions-body",
            ))));
        for source in &self.sources {
            for service in [Service::Slack, Service::Discord] {
                let Some((_, _, presence)) = self
                    .detected
                    .iter()
                    .find(|(id, kind, _)| id == &source.id && *kind == service)
                else {
                    continue;
                };
                if *presence == Presence::NeedsAccess {
                    body = body.push(Box::new(Text::new(&super::t_args(
                        if source.supports_encrypted_cookie_capture() {
                            "installer-service-source-needs-access"
                        } else {
                            "installer-service-source-reconnect-required"
                        },
                        &[("source", source.label.clone().into())],
                    ))));
                }
                if *presence == Presence::NeedsAccess && !source.supports_encrypted_cookie_capture()
                {
                    continue;
                }
                let chosen = ui::LocalState::new(
                    self.included
                        .iter()
                        .any(|(id, kind)| id == &source.id && *kind == service),
                );
                let label = super::t_args(
                    match service {
                        Service::Slack => "installer-service-source-slack",
                        Service::Discord if source.material == Material::DiscordOrigin => {
                            "installer-service-source-discord-origin"
                        }
                        Service::Discord => "installer-service-source-discord",
                    },
                    &[("source", source.label.clone().into())],
                );
                let id = source.id.clone();
                let change = bindings.clone();
                body = body.push(Box::new(
                    Checkbox::from_state(&chosen)
                        .with_id(&format!(
                            "installer.session.{}.{}",
                            service.as_str(),
                            source.id
                        ))
                        .with_label(label)
                        .on_change(move |included| change(id.clone(), service, included)),
                ));
            }
        }
        Box::new(body)
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn empty_selection_cannot_invent_a_source() {
        let mut selection = Selection::default();
        assert!(!selection.available());
        assert_eq!(
            selection.set("ungranted-source", Service::Discord, true),
            Err("installer-service-source-changed"),
        );
        assert!(selection.selected_for_accounts(true).is_none());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::setup::account_transfer::{self, SourceRoots};
    use std::os::unix::fs::PermissionsExt;

    pub(super) fn firefox(home: &Path, name: &str, with_slack: bool) -> rusqlite::Connection {
        let root = home.join(".mozilla/firefox");
        let profile = root.join(name);
        std::fs::create_dir_all(&profile).unwrap();
        let connection = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=17;
            CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,
            expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,
            originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER);").unwrap();
        if with_slack {
            connection
                .execute_batch(
                    "INSERT INTO moz_cookies VALUES(1,'.slack.com','d',
                'private-synthetic-sentinel','/',2000000000000,1,1,256,'',0,0);",
                )
                .unwrap();
        } else {
            connection
                .execute_batch(
                    "INSERT INTO moz_cookies VALUES(1,'unrelated.example','x',
                'private-unrelated-sentinel','/',2000000000000,1,1,256,'',0,0);",
                )
                .unwrap();
        }
        connection
    }
    pub(super) fn inventory(home: &Path, names: &[&str]) -> account_transfer::Inventory {
        use std::fmt::Write as _;
        let root = home.join(".mozilla/firefox");
        let index = names
            .iter()
            .enumerate()
            .fold(String::new(), |mut index, (i, name)| {
                writeln!(
                    index,
                    "[Profile{i}]\nName={name}\nIsRelative=1\nPath={name}"
                )
                .unwrap();
                index
            });
        std::fs::write(root.join("profiles.ini"), index).unwrap();
        account_transfer::discover(&SourceRoots {
            home: home.to_owned(),
            config: home.join(".config"),
        })
    }
    fn bytes(home: &Path, names: &[&str]) -> Vec<Vec<u8>> {
        names
            .iter()
            .flat_map(|name| {
                ["cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm"].map(|file| {
                    std::fs::read(home.join(".mozilla/firefox").join(name).join(file)).unwrap()
                })
            })
            .collect()
    }

    pub(super) fn native_slack_database(
        home: &Path,
        flatpak: bool,
        encrypted: bool,
        eligible: bool,
    ) -> (std::path::PathBuf, rusqlite::Connection) {
        let profile = home.join(if flatpak {
            ".var/app/com.slack.Slack/config/Slack"
        } else {
            ".config/Slack"
        });
        std::fs::create_dir_all(profile.join("Network")).unwrap();
        let path = profile.join("Network/Cookies");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE meta(key TEXT,value TEXT); INSERT INTO meta VALUES('version','24');
            CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,
            is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,
            encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER,
            creation_utc INTEGER);",
            )
            .unwrap();
        let host = if eligible {
            ".slack.com"
        } else {
            "unrelated.example"
        };
        let value = if encrypted {
            ""
        } else {
            "private-native-slack-sentinel"
        };
        let cipher = if encrypted {
            [b"v11".as_slice(), &[0; 16]].concat()
        } else {
            Vec::new()
        };
        connection
            .execute(
                "INSERT INTO cookies VALUES(?1,'d',?2,'/',0,1,1,-1,0,?3,'',2,443,1)",
                rusqlite::params![host, value, cipher],
            )
            .unwrap();
        (path, connection)
    }
    fn native_slack_bytes(path: &Path) -> Vec<Vec<u8>> {
        [
            path.to_path_buf(),
            path.with_file_name("Cookies-wal"),
            path.with_file_name("Cookies-shm"),
        ]
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect()
    }
    #[test]
    fn native_and_flatpak_slack_selected_cookies_use_real_capture_and_sealed_custody() {
        for flatpak in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let (path, _database) = native_slack_database(home.path(), flatpak, false, true);
            let inventory = account_transfer::discover(&SourceRoots {
                home: home.path().into(),
                config: home.path().join(".config"),
            });
            assert!(
                inventory
                    .candidates
                    .iter()
                    .any(|candidate| candidate.provider == account_transfer::Provider::Slack)
            );
            let before = native_slack_bytes(&path);
            let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
            assert_eq!(selection.detected.len(), 1);
            assert_eq!(selection.detected[0].1, Service::Slack);
            assert_eq!(selection.detected[0].2, Presence::Readable);
            assert_eq!(selection.included.len(), 1);
            assert!(
                !selection
                    .report()
                    .to_string()
                    .contains("private-native-slack-sentinel")
            );
            let prepared = selection
                .selected()
                .capture([7; 32], "native-slack-operation".into())
                .unwrap();
            let (artifact, declaration) = prepared
                .seal(b"private synthetic transfer password")
                .unwrap();
            assert_eq!(declaration.sessions.len(), 1);
            assert_eq!(declaration.sessions[0].service, Service::Slack);
            // Canonical import creates private custody only beneath an already
            // owner-private parent; default tempfile directory modes are not
            // that authority. Keep the wrong-identity target-absence check.
            let target = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir()
                .unwrap();
            assert_eq!(
                std::fs::metadata(target.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let destination = target.path().join("custody");
            assert!(
                crate::setup::service_sessions::import(
                    &artifact,
                    &declaration,
                    b"private synthetic transfer password",
                    [8; 32],
                    "native-slack-operation",
                    &destination
                )
                .is_err()
            );
            assert!(!destination.exists());
            crate::setup::service_sessions::import(
                &artifact,
                &declaration,
                b"private synthetic transfer password",
                [7; 32],
                "native-slack-operation",
                &destination,
            )
            .unwrap();
            assert_eq!(
                std::fs::metadata(&destination)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(target.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let custodian = account_custody::Custodian::for_root(Some(destination.clone()));
            let imported = custodian
                .service_session(
                    &accounts::AccountId::from(declaration.sessions[0].id.clone()),
                    [7; 32],
                    "native-slack-operation",
                )
                .unwrap();
            assert_eq!(imported.cookies.len(), 1);
            assert_eq!(imported.cookies[0].value, "private-native-slack-sentinel");
            assert!(imported.origin_storage.is_empty());
            assert_eq!(native_slack_bytes(&path), before);
        }
    }
    #[test]
    fn native_slack_empty_and_encrypted_material_never_become_readable_or_chrome_authorized() {
        for eligible in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let (path, _database) = native_slack_database(home.path(), false, true, eligible);
            let inventory = account_transfer::discover(&SourceRoots {
                home: home.path().into(),
                config: home.path().join(".config"),
            });
            let before = native_slack_bytes(&path);
            let mut selection = Selection::from_discovery(
                service_profiles::discover_without_slack_package(
                    home.path(),
                    &inventory.candidates,
                )
                .unwrap(),
            )
            .unwrap();
            assert!(selection.selected().is_empty());
            if eligible {
                assert_eq!(selection.detected.len(), 1);
                assert_eq!(selection.detected[0].2, Presence::NeedsAccess);
                let id = selection.detected[0].0.clone();
                assert_eq!(
                    selection.set(&id, Service::Slack, true),
                    Err("installer-service-reconnect-required")
                );
                assert!(selection.set(&id, Service::Discord, true).is_err());
                assert!(selection.selected().is_empty());
                assert!(
                    selection.decided.is_empty(),
                    "refused inclusion does not hide attention"
                );
                assert_eq!(selection.report()["sources"][0]["selectable"], false);
                // Preserve the independent real adapter refusal even though the
                // ordinary selection action now prevents unsupported capture.
                let unsupported = SelectedSources {
                    sources: vec![(
                        Arc::clone(
                            selection
                                .sources
                                .iter()
                                .find(|source| source.id == id)
                                .unwrap(),
                        ),
                        Service::Slack,
                    )],
                };
                assert!(
                    unsupported
                        .capture([7; 32], "native-slack-operation".into())
                        .is_err()
                );
                let source_label = selection
                    .sources
                    .iter()
                    .find(|source| source.id == id)
                    .unwrap()
                    .label
                    .clone();
                let mut app = consent_app(selection);
                assert!(
                    app.effect(
                        &super::super::InstallerPreviewEffect::SetPreviewServiceSession {
                            source_id: id.clone(),
                            service: "slack".into(),
                            included: true,
                        },
                    )
                    .is_err()
                );
                app.settle();
                app.harness().resize((1180, 820)).unwrap();
                super::super::visual_tests::paint_app(
                    &mut app,
                    "installer-native-slack-reconnect",
                    &["installer.details", "installer.back", "installer.next"],
                    (1180, 820),
                    "dark",
                );
                let rendered = app.harness().inspection_json().unwrap();
                assert!(rendered.contains(&super::super::t_args(
                    "installer-service-source-reconnect-required",
                    &[("source", source_label.into())]
                )));
                assert!(!rendered.contains(&format!("installer.session.slack.{id}")));
                assert!(!rendered.contains("current computer’s keychain"));
                assert!(!rendered.contains("private-native-slack-sentinel"));
                assert!(!rendered.contains(home.path().to_str().unwrap()));
                app.with_widget::<super::super::InstallerPreview, _>(|preview| {
                    assert!(preview.belongings.sessions.selected().is_empty());
                    assert!(preview.belongings.sessions.decided.is_empty());
                })
                .unwrap();
                // Recover the public report assertion subject after the actual action.
                selection = Selection::from_discovery(
                    service_profiles::discover_without_slack_package(
                        home.path(),
                        &inventory.candidates,
                    )
                    .unwrap(),
                )
                .unwrap();
                assert!(
                    !selection
                        .report()
                        .to_string()
                        .contains("private-native-slack-sentinel")
                );
            } else {
                assert!(selection.detected.is_empty());
            }
            assert_eq!(native_slack_bytes(&path), before);
        }
    }

    #[test]
    fn unsafe_native_source_keeps_independent_firefox_selected_and_warning() {
        let home = tempfile::tempdir().unwrap();
        let _firefox = firefox(home.path(), "independent", true);
        inventory(home.path(), &["independent"]);
        let (path, _slack) = native_slack_database(home.path(), false, false, true);
        let candidates = account_transfer::discover(&SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        assert!(
            candidates
                .candidates
                .iter()
                .any(|candidate| candidate.provider == account_transfer::Provider::Slack)
        );
        assert!(
            candidates
                .candidates
                .iter()
                .any(|candidate| candidate.provider == account_transfer::Provider::Firefox)
        );
        let original = native_slack_bytes(&path);
        let firefox_bytes = std::fs::read(
            home.path()
                .join(".mozilla/firefox/independent/cookies.sqlite"),
        )
        .unwrap();
        // Actual source custody becomes inadmissible after metadata inventory.
        // No discovery callback or permission mock substitutes for the real walk.
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        let selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
        assert!(selection.unavailable());
        assert_eq!(selection.included.len(), 1);
        let selected_id = &selection.included[0].0;
        let selected = selection
            .sources
            .iter()
            .find(|source| &source.id == selected_id)
            .unwrap();
        assert_eq!(
            selected.provider,
            imager_backend::browser_selection::Provider::Firefox
        );
        assert_eq!(selection.included[0].1, Service::Slack);
        let captured = selection
            .selected()
            .capture([7; 32], "independent-source-operation".into())
            .unwrap();
        let (_, declaration) = captured
            .seal(b"private synthetic isolation password")
            .unwrap();
        assert_eq!(declaration.sessions.len(), 1);
        let public = selection.report().to_string();
        assert!(!public.contains(home.path().to_str().unwrap()));
        assert!(!public.contains("private-native-slack-sentinel"));
        assert!(!public.contains("private-synthetic-sentinel"));
        assert_eq!(native_slack_bytes(&path), original);
        assert_eq!(
            std::fs::read(
                home.path()
                    .join(".mozilla/firefox/independent/cookies.sqlite")
            )
            .unwrap(),
            firefox_bytes
        );
        let guarded_inventory =
            service_profiles::discover(home.path(), &candidates.candidates).unwrap();
        guarded_inventory.validate_home().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            guarded_inventory.validate_home().is_err(),
            "the held guard used around actual presence observes in-flight Home custody loss"
        );
        assert!(
            Selection::discover(home.path(), &candidates.candidates).is_err(),
            "global Home custody must remain fatal"
        );
    }

    #[test]
    fn migrate_all_defaults_only_actual_supported_material_and_preserves_original_wal() {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let _first = firefox(home.path(), "first", true);
        let _second = firefox(home.path(), "second", true);
        let _empty = firefox(home.path(), "unrelated", false);
        let inventory = inventory(home.path(), &["second", "unrelated", "first"]);
        let before = bytes(home.path(), &["first", "second", "unrelated"]);
        let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert_eq!(selection.detected.len(), 2);
        assert_eq!(
            selection.included.len(),
            1,
            "one source per service/material"
        );
        assert_eq!(selection.included[0].1, Service::Slack);
        assert_eq!(
            selection.included[0].0,
            selection
                .detected
                .iter()
                .map(|(id, _, _)| id)
                .min()
                .unwrap()
                .clone()
        );
        assert!(
            selection
                .detected
                .iter()
                .all(|(_, _, presence)| *presence == Presence::Readable)
        );
        let mut reversed = inventory.candidates;
        reversed.reverse();
        assert_eq!(
            Selection::discover(home.path(), &reversed)
                .unwrap()
                .included,
            selection.included
        );
        assert_eq!(
            bytes(home.path(), &["first", "second", "unrelated"]),
            before
        );
        let report = selection.report().to_string();
        assert!(!report.contains("private-synthetic-sentinel"));
        assert!(!report.contains("private-unrelated-sentinel"));
        assert!(!report.contains("source_snapshot_digest"));
        let prepared = selection
            .selected()
            .capture([7; 32], "operation".into())
            .unwrap();
        let (_, declaration) = prepared.seal(b"synthetic-checkpoint-password").unwrap();
        assert_eq!(declaration.sessions.len(), 1);
    }
    #[test]
    fn database_container_without_selected_service_material_is_not_an_included_login() {
        let home = tempfile::tempdir().unwrap();
        let _empty = firefox(home.path(), "unrelated", false);
        let inventory = inventory(home.path(), &["unrelated"]);
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert!(!selection.available());
        assert!(selection.selected().is_empty());
        let id = selection.sources[0].id.clone();
        assert!(selection.set(&id, Service::Slack, true).is_err());
        assert!(selection.set(&id, Service::Discord, true).is_err());
    }
    #[test]
    fn default_inclusion_exclusion_and_cancel_preserve_exact_reviewed_baseline() {
        let home = tempfile::tempdir().unwrap();
        let _source = firefox(home.path(), "included", true);
        let inventory = inventory(home.path(), &["included"]);
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        let baseline = selection.included.clone();
        let id = baseline[0].0.clone();
        selection.begin_edit();
        assert!(selection.set(&id, Service::Slack, false).unwrap());
        selection.cancel_edit();
        assert_eq!(selection.included, baseline);
        selection.begin_edit();
        selection.set(&id, Service::Slack, false).unwrap();
        assert!(selection.commit_edit());
        assert!(selection.selected().is_empty());
        assert!(
            selection
                .set("foreign-source", Service::Slack, true)
                .is_err()
        );
    }
    #[test]
    fn malformed_origin_container_never_becomes_default_or_a_selectable_login() {
        let home = tempfile::tempdir().unwrap();
        let store = home.path().join(".config/discord/Local Storage/leveldb");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("CURRENT"), b"private-invalid-origin-sentinel").unwrap();
        let inventory = account_transfer::discover(&SourceRoots {
            home: home.path().to_owned(),
            config: home.path().join(".config"),
        });
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert!(!selection.available());
        assert!(selection.selected().is_empty());
        let id = selection.sources[0].id.clone();
        assert!(selection.set(&id, Service::Discord, true).is_err());
        assert_eq!(
            std::fs::read(store.join("CURRENT")).unwrap(),
            b"private-invalid-origin-sentinel"
        );
        assert!(
            !selection
                .report()
                .to_string()
                .contains("private-invalid-origin-sentinel")
        );
    }
    #[test]
    fn migrate_all_includes_current_discord_origin_without_guessing_cookie_material() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".config/discord");
        crate::setup::service_sessions::source::discord_origin::fixture_selected_origin(&profile);
        let inventory = account_transfer::discover(&SourceRoots {
            home: home.path().to_owned(),
            config: home.path().join(".config"),
        });
        let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert_eq!(selection.included.len(), 1);
        assert_eq!(selection.included[0].1, Service::Discord);
        assert_eq!(selection.sources[0].material, Material::DiscordOrigin);
        assert!(!profile.join("Cookies").exists());
        assert!(
            !selection
                .report()
                .to_string()
                .contains("private-origin-sentinel")
        );
        let prepared = selection
            .selected()
            .capture([7; 32], "operation".into())
            .unwrap();
        let (_, declaration) = prepared.seal(b"synthetic-checkpoint-password").unwrap();
        assert_eq!(declaration.consumer_protocol, 2);
        assert_eq!(declaration.sessions.len(), 1);
    }
    #[test]
    fn public_belongings_action_can_exclude_a_default_included_detected_session() {
        use ui::framework::testing::TestApp;
        let home = tempfile::tempdir().unwrap();
        let _source = firefox(home.path(), "included", true);
        let inventory = inventory(home.path(), &["included"]);
        let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        let id = selection.included[0].0.clone();
        let mut app = TestApp::<crate::ImagerShell>::launched(&["--installer-preview"]).unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.restart = super::super::restart_controller::Restart::default();
            preview.journey.page = super::super::Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.details = true;
            preview.journey.categories = [false, false, true];
            preview.belongings.sessions = selection;
            preview.dirty = true;
        })
        .unwrap();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("private-synthetic-sentinel")
        );
        app.effect(
            &super::super::InstallerPreviewEffect::SetPreviewServiceSession {
                source_id: id.clone(),
                service: "slack".into(),
                included: false,
            },
        )
        .unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert!(preview.belongings.sessions.selected().is_empty());
            assert_eq!(preview.journey.categories, [false, false, true]);
        })
        .unwrap();
        app.effect(
            &super::super::InstallerPreviewEffect::SetPreviewServiceSession {
                source_id: id,
                service: "slack".into(),
                included: true,
            },
        )
        .unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert_eq!(preview.belongings.sessions.included.len(), 1);
        })
        .unwrap();
    }
    fn encrypted_chromium(home: &Path) -> rusqlite::Connection {
        let profile = home.join(".config/chromium/Default");
        std::fs::create_dir_all(&profile).unwrap();
        let database = rusqlite::Connection::open(profile.join("Cookies")).unwrap();
        database
            .execute_batch(
                "CREATE TABLE meta(key TEXT,value TEXT);
            INSERT INTO meta VALUES('version','24');
            CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,
            is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,
            encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER,
            creation_utc INTEGER);",
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO cookies VALUES('.slack.com','d','','/',0,1,1,-1,0,?1,'',2,443,1)",
                rusqlite::params![[b"v11".as_slice(), &[0; 16]].concat()],
            )
            .unwrap();
        database
    }
    #[test]
    fn encrypted_chromium_material_is_visible_for_explicit_access_without_discovery_decryption() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".config/chromium/Default");
        let _database = encrypted_chromium(home.path());
        let roots = SourceRoots {
            home: home.path().to_owned(),
            config: home.path().join(".config"),
        };
        let inventory = account_transfer::discover(&roots);
        let before = std::fs::read(profile.join("Cookies")).unwrap();
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert_eq!(selection.detected.len(), 1);
        assert_eq!(selection.detected[0].1, Service::Slack);
        assert_eq!(selection.detected[0].2, Presence::NeedsAccess);
        assert!(
            selection.selected().is_empty(),
            "undecrypted input is not proved readable"
        );
        assert!(selection.report().to_string().contains("needs_access"));
        let id = selection.detected[0].0.clone();
        assert!(selection.set(&id, Service::Slack, true).unwrap());
        assert!(selection.set(&id, Service::Discord, true).is_err());
        assert_eq!(std::fs::read(profile.join("Cookies")).unwrap(), before);
    }
    #[test]
    fn overview_reports_selected_material_and_only_selected_permission_attention() {
        let home = tempfile::tempdir().unwrap();
        let source = firefox(home.path(), "readable", true);
        source.execute_batch("INSERT INTO moz_cookies VALUES(2,'.discord.com','__dcfduid','private-discord-sentinel','/',2000000000000,1,1,256,'',0,0);").unwrap();
        let _encrypted = encrypted_chromium(home.path());
        let inventory = inventory(home.path(), &["readable"]);
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        let discord = selection
            .included
            .iter()
            .find(|(_, service)| *service == Service::Discord)
            .unwrap()
            .clone();
        assert!(
            selection
                .overview(true)
                .contains(&super::super::t("installer-service-discord-label"))
        );
        selection.set(&discord.0, discord.1, false).unwrap();
        assert!(
            !selection
                .overview(true)
                .contains(&super::super::t("installer-service-discord-label"))
        );
        assert!(
            selection
                .detected
                .iter()
                .any(|(_, service, _)| *service == Service::Discord),
            "deselected detected material must remain available in details"
        );
        let selected = selection.included.clone();
        let readable_summary = selection.overview(true);
        assert!(readable_summary.contains(&super::super::t("installer-service-slack-label")));
        assert!(!readable_summary.contains("permission"));
        assert!(!readable_summary.contains("needs access:"));
        assert!(!readable_summary.contains("included:"));
        assert_eq!(selection.included, selected);
        let blocked = selection
            .detected
            .iter()
            .find(|(_, _, presence)| *presence == Presence::NeedsAccess)
            .unwrap()
            .clone();
        selection.set(&blocked.0, blocked.1, true).unwrap();
        let explicit = selection.included.clone();
        let permission = super::super::t_args(
            "installer-service-sessions-selected-needs-access",
            &[(
                "services",
                super::super::t("installer-service-slack-label").into(),
            )],
        );
        assert!(selection.overview(true).contains(&permission));
        assert_eq!(selection.included, explicit);
        assert_eq!(
            selection.overview(false),
            super::super::t("installer-service-sessions-accounts-excluded")
        );
        assert_eq!(selection.included, explicit);
        for (id, service) in explicit {
            selection.set(&id, service, false).unwrap();
        }
        assert_eq!(
            selection.overview(true),
            super::super::t("installer-service-sessions-none-selected")
        );
        assert!(selection.included.is_empty());
        assert!(
            !selection
                .overview(true)
                .contains("private-synthetic-sentinel")
        );
        assert!(
            !selection
                .overview(true)
                .contains(home.path().to_str().unwrap())
        );
    }

    #[test]
    fn collapsed_website_data_overview_renders_readable_and_attention_sources_and_opens_existing_details()
     {
        use ui::framework::testing::TestApp;
        for attention in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let _source = firefox(home.path(), "included", true);
            let mut inventory = inventory(home.path(), &["included"]);
            if attention {
                let _encrypted = encrypted_chromium(home.path());
                inventory = account_transfer::discover(&SourceRoots {
                    home: home.path().to_owned(),
                    config: home.path().join(".config"),
                });
            }
            let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
            assert_eq!(selection.included.len(), 1);
            let expected = super::super::t_args(
                "installer-service-sessions-overview",
                &[(
                    "services",
                    super::super::t("installer-service-slack-label").into(),
                )],
            );
            assert_eq!(selection.overview(true), expected);
            let mut app =
                TestApp::<crate::ImagerShell>::launched(&["--installer-preview"]).unwrap();
            app.with_widget::<super::super::InstallerPreview, _>(|preview| {
                preview.live = true;
                preview.restart = super::super::restart_controller::Restart::default();
                preview.journey.page = super::super::Page::Belongings;
                preview.journey.migrate = Some(true);
                preview.journey.details = false;
                preview.journey.categories = [false, false, true];
                preview.belongings.sessions = selection;
                preview.dirty = true;
            })
            .unwrap();
            for (tag, theme) in [("dark", ui::Theme::dark()), ("light", ui::Theme::light())] {
                app.harness().resize((1180, 820)).unwrap();
                app.harness().set_theme(theme.with_reduce_motion(true));
                super::super::visual_tests::paint_app(
                    &mut app,
                    if attention {
                        "installer-session-overview-needs-access"
                    } else {
                        "installer-session-overview-readable"
                    },
                    &[
                        "installer.sessions.overview",
                        "installer.details",
                        "installer.next",
                    ],
                    (1180, 820),
                    tag,
                );
                let view = app.harness().inspection_json().unwrap();
                assert!(view.contains(&expected));
                assert!(!view.contains("private-synthetic-sentinel"));
                assert!(!view.contains(home.path().to_str().unwrap()));
            }
            app.effect(&super::super::InstallerPreviewEffect::TogglePreviewDetails)
                .unwrap();
            app.with_widget::<super::super::InstallerPreview, _>(|preview| {
                assert!(preview.journey.details);
                assert_eq!(preview.belongings.sessions.included.len(), 1);
            })
            .unwrap();
            app.settle();
            if attention {
                assert!(
                    app.harness()
                        .inspection_json()
                        .unwrap()
                        .contains(&super::super::t_args(
                            "installer-service-source-needs-access",
                            &[("source", "Chromium · Default".into())]
                        ))
                );
            }
        }
    }
    #[test]
    fn initial_encrypted_attention_is_rendered_and_explicit_exclusion_survives_cancel() {
        for mixed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let _encrypted = encrypted_chromium(home.path());
            let roots = SourceRoots {
                home: home.path().to_owned(),
                config: home.path().join(".config"),
            };
            if mixed {
                let readable = firefox(home.path(), "discord", false);
                readable.execute_batch("INSERT INTO moz_cookies VALUES(2,'.discord.com','__dcfduid','private-overview-discord-sentinel','/',2000000000000,1,1,256,'',0,0);").unwrap();
                inventory(home.path(), &["discord"]);
            }
            let original =
                std::fs::read(home.path().join(".config/chromium/Default/Cookies")).unwrap();
            let candidates = account_transfer::discover(&roots);
            let mut selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
            let source_id = selection
                .detected
                .iter()
                .find(|(_, service, presence)| {
                    *service == Service::Slack && *presence == Presence::NeedsAccess
                })
                .unwrap()
                .0
                .clone();
            let found = super::super::t_args(
                "installer-service-sessions-found-attention",
                &[(
                    "services",
                    super::super::t("installer-service-slack-label").into(),
                )],
            );
            let selected = if mixed {
                super::super::t_args(
                    "installer-service-sessions-overview",
                    &[(
                        "services",
                        super::super::t("installer-service-discord-label").into(),
                    )],
                )
            } else {
                super::super::t("installer-service-sessions-none-selected")
            };
            let expected = format!("{selected} {found}");
            assert_eq!(selection.overview(true), expected);
            assert_eq!(
                selection.overview(false),
                super::super::t("installer-service-sessions-accounts-excluded")
            );
            selection.begin_edit();
            assert!(
                selection.set(&source_id, Service::Slack, false).unwrap(),
                "explicit exclusion changes the initial attention decision"
            );
            assert_eq!(selection.overview(true), selected);
            selection.cancel_edit();
            assert_eq!(selection.overview(true), expected);
            let mut app = consent_app(selection);
            app.with_widget::<super::super::InstallerPreview, _>(|preview| {
                preview.journey.details = false;
                preview.dirty = true;
            })
            .unwrap();
            for (tag, theme) in [("dark", ui::Theme::dark()), ("light", ui::Theme::light())] {
                app.harness().resize((1180, 820)).unwrap();
                app.harness().set_theme(theme.with_reduce_motion(true));
                super::super::visual_tests::paint_app(
                    &mut app,
                    "installer-session-initial-attention",
                    &[
                        "installer.sessions.overview",
                        "installer.details",
                        "installer.next",
                    ],
                    (1180, 820),
                    tag,
                );
                let rendered = app.harness().inspection_json().unwrap();
                assert!(rendered.contains(&expected));
                assert!(!rendered.contains(home.path().to_str().unwrap()));
                assert!(!rendered.contains("private-overview-discord-sentinel"));
            }
            app.effect(&super::super::InstallerPreviewEffect::TogglePreviewDetails)
                .unwrap();
            app.effect(
                &super::super::InstallerPreviewEffect::SetPreviewServiceSession {
                    source_id: source_id.clone(),
                    service: "slack".into(),
                    included: false,
                },
            )
            .unwrap();
            app.effect(&super::super::InstallerPreviewEffect::TogglePreviewDetails)
                .unwrap();
            app.settle();
            let rendered = app.harness().inspection_json().unwrap();
            assert!(rendered.contains(&selected));
            assert!(!rendered.contains(&found));
            app.with_widget::<super::super::InstallerPreview, _>(|preview| {
                assert_eq!(preview.belongings.sessions.overview(true), selected);
                assert!(
                    !preview
                        .belongings
                        .sessions
                        .included
                        .iter()
                        .any(|(_, service)| *service == Service::Slack)
                );
            })
            .unwrap();
            assert_eq!(
                std::fs::read(home.path().join(".config/chromium/Default/Cookies")).unwrap(),
                original
            );
        }
    }

    fn consent_app(selection: Selection) -> ui::framework::testing::TestApp<crate::ImagerShell> {
        use ui::framework::testing::TestApp;
        let mut app = TestApp::<crate::ImagerShell>::launched(&["--installer-preview"]).unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.restart = super::super::restart_controller::Restart::default();
            preview.journey.page = super::super::Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.details = true;
            preview.journey.categories = [false, false, true];
            preview.belongings.sessions = selection;
            preview.dirty = true;
        })
        .unwrap();
        app
    }
    #[test]
    fn public_accounts_exclusion_hides_session_transfer_and_review_then_restores_granular_choices()
    {
        let home = tempfile::tempdir().unwrap();
        let _source = firefox(home.path(), "included", true);
        let inventory = inventory(home.path(), &["included"]);
        let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        let remembered = selection.included.clone();
        let mut app = consent_app(selection);
        app.effect(&super::super::InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: false,
        })
        .unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert_eq!(preview.belongings.sessions.included, remembered);
            assert!(
                preview
                    .belongings
                    .sessions
                    .selected_for_accounts(preview.journey.categories[2])
                    .is_none()
            );
            assert!(
                preview
                    .belongings
                    .sessions
                    .review_labels(preview.journey.categories[2])
                    .is_empty()
            );
        })
        .unwrap();
        assert!(
            app.effect(
                &super::super::InstallerPreviewEffect::SetPreviewServiceSession {
                    source_id: remembered[0].0.clone(),
                    service: "slack".into(),
                    included: true,
                }
            )
            .is_err(),
            "parent exclusion cannot be bypassed by a granular semantic action"
        );
        app.settle();
        let view = app.harness().inspection_json().unwrap();
        assert!(view.contains(&super::super::t(
            "installer-service-sessions-accounts-excluded"
        )));
        assert!(!view.contains(&format!("installer.session.slack.{}", remembered[0].0)));
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            preview.journey.page = super::super::Page::Review;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.review.session.0")
        );
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            preview.journey.page = super::super::Page::Belongings;
            preview.dirty = true;
        })
        .unwrap();
        app.effect(&super::super::InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: true,
        })
        .unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert_eq!(preview.belongings.sessions.included, remembered);
            assert!(
                preview
                    .belongings
                    .sessions
                    .selected_for_accounts(preview.journey.categories[2])
                    .is_some()
            );
            preview.journey.page = super::super::Page::Review;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.review.session.0")
        );
    }
    #[test]
    fn cancelling_parent_accounts_edit_preserves_website_material_choices() {
        let home = tempfile::tempdir().unwrap();
        let _source = firefox(home.path(), "included", true);
        let inventory = inventory(home.path(), &["included"]);
        let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        let remembered = selection.included.clone();
        let mut app = consent_app(selection);
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            preview.journey.page = super::super::Page::Review;
            preview.dirty = true;
        })
        .unwrap();
        app.effect(&super::super::InstallerPreviewEffect::EditPreview {
            section: "belongings-details".into(),
        })
        .unwrap();
        app.effect(&super::super::InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: false,
        })
        .unwrap();
        app.effect(&super::super::InstallerPreviewEffect::BackInPreview)
            .unwrap();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, super::super::Page::Review);
            assert!(preview.journey.categories[2]);
            assert_eq!(preview.belongings.sessions.included, remembered);
            assert!(
                preview
                    .belongings
                    .sessions
                    .selected_for_accounts(true)
                    .is_some()
            );
        })
        .unwrap();
    }
}

#[cfg(all(test, target_os = "linux", feature = "native-slack-qualification"))]
mod slack_native;
