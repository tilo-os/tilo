//! Windows prepares selected local data and private sign-in suggestions only.
//! Identity enrollment and account authorization happen later in the live OS.
use super::*;
use crate::setup::windows_inventory::{Decision, Inventory, RootRole, SelectedRoot};
use std::path::Path;

enum Discovery {
    Suggestions(Vec<crate::setup::windows_inventory::RootSuggestion>),
    Granted {
        selected: Vec<SelectedRoot>,
        belongings: belongings::Belongings,
        hints: Vec<account_custody::hints::AccountHint>,
        hints_unavailable: bool,
    },
}

pub(super) struct Preparation {
    pub enabled: bool,
    selected: Vec<SelectedRoot>,
    suggestions: Vec<crate::setup::windows_inventory::RootSuggestion>,
    hints: Vec<account_custody::hints::AccountHint>,
    password: ui::LocalState<String>,
    exclusion_reason: ui::LocalState<String>,
    hints_unavailable: bool,
    picker: Option<(Option<RootRole>, ui::PortalChannel)>,
    worker: Option<std::sync::mpsc::Receiver<Result<crate::windows_media::WindowsMedia, String>>>,
    prepared_media: Option<crate::windows_media::WindowsMedia>,
    source_worker: Option<std::sync::mpsc::Receiver<Result<Discovery, String>>>,
    exported: bool,
    failed: bool,
}

impl Default for Preparation {
    fn default() -> Self {
        Self {
            enabled: cfg!(windows),
            selected: Vec::new(),
            suggestions: Vec::new(),
            hints: Vec::new(),
            password: ui::LocalState::new(String::new()),
            exclusion_reason: ui::LocalState::new(String::new()),
            hints_unavailable: false,
            picker: None,
            worker: None,
            prepared_media: None,
            source_worker: None,
            exported: false,
            failed: false,
        }
    }
}

impl Preparation {
    pub(super) fn media_handoff(&self) -> Option<crate::windows_media::WindowsMedia> {
        self.exported.then(|| self.prepared_media.clone()).flatten()
    }

    pub(super) fn projection(&self) -> serde_json::Value {
        serde_json::json!({"available": self.enabled, "exporting": self.worker.is_some(), "discovering": self.source_worker.is_some(),
            "exported": self.exported, "failed": self.failed, "hints_unavailable":self.hints_unavailable,
            "sign_in": "live_only", "replacement_authorized":false})
    }
    pub(super) fn busy(&self) -> bool {
        self.worker.is_some() || self.source_worker.is_some()
    }
    pub(super) fn busy_label(&self) -> &'static str {
        if self.source_worker.is_some() {
            "installer-windows-checking"
        } else {
            "installer-windows-exporting"
        }
    }
    pub(super) fn choosing(&self) -> bool {
        self.picker.is_some()
    }
}

fn role(value: &str) -> Result<RootRole, &'static str> {
    match value {
        "known-folder" => Ok(RootRole::KnownFolder),
        "onedrive" => Ok(RootRole::OneDrive),
        "outlook" => Ok(RootRole::Outlook),
        "steam" => Ok(RootRole::Steam),
        "steam-library" => Ok(RootRole::SteamLibrary),
        "other" => Ok(RootRole::Other),
        _ => Err("installer-preview-invalid-choice"),
    }
}

impl InstallerPreview {
    pub(super) fn apply_windows(
        &mut self,
        effect: InstallerPreviewEffect,
    ) -> Result<(), &'static str> {
        use InstallerPreviewEffect::{
            ChooseWindowsExport, ChooseWindowsRoot, ExportWindowsPreparation, FindWindowsRoots,
            PrepareUsb, ReadPreview, ReviewWindowsCategory, ReviewWindowsRoot, SelectWindowsRoot,
            SetWindowsExclusionReason, SetWindowsPreparationPassword,
        };
        if self.windows.busy() && !matches!(effect, ReadPreview) {
            return Err("installer-preview-transfer-busy");
        }
        match effect {
            ReadPreview => {}
            FindWindowsRoots => {
                self.windows.picker = None;
                let (tx, rx) = std::sync::mpsc::channel();
                self.windows.source_worker = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(
                        super::super::windows_inventory::suggestions().map(Discovery::Suggestions),
                    );
                });
            }
            ChooseWindowsRoot { role: value } => {
                let role = role(&value)?;
                self.windows.picker = Some((
                    Some(role),
                    ui::PickRequest::directory(
                        &t("installer-windows-source-title"),
                        &t("installer-windows-grant"),
                    )
                    .send(),
                ));
            }
            SelectWindowsRoot {
                source,
                role: value,
            } => {
                let grant = SelectedRoot {
                    path: source.into(),
                    role: role(&value)?,
                };
                let mut selected = self.windows.selected.clone();
                selected.retain(|existing| existing.path != grant.path);
                selected.push(grant);
                self.windows.picker = None;
                let (tx, rx) = std::sync::mpsc::channel();
                self.windows.source_worker = Some(rx);
                // Inventory and bounded native metadata queries never block rendering.
                std::thread::spawn(move || {
                    let result =
                        belongings::Belongings::from_windows_roots(&selected).map(|belongings| {
                            let profiles =
                                super::super::account_transfer::native_windows_profiles();
                            let hints_unavailable = profiles.is_err();
                            let hints = super::super::account_transfer::private_windows_hints(
                                &selected,
                                &profiles.unwrap_or_default(),
                            );
                            Discovery::Granted {
                                selected,
                                belongings,
                                hints,
                                hints_unavailable,
                            }
                        });
                    let _ = tx.send(result);
                });
                self.windows.exported = false;
            }
            ReviewWindowsRoot {
                source_id,
                decision,
                reason,
            } => {
                let decision = match decision.as_str() {
                    "copy" => Decision::Copy,
                    "retain-original" => Decision::RetainOriginal,
                    "exclude" => Decision::Exclude,
                    _ => return Err("installer-preview-invalid-choice"),
                };
                self.belongings
                    .windows
                    .as_mut()
                    .ok_or("installer-windows-source-failed")?
                    .select(&source_id, decision, reason)
                    .map_err(|_| "installer-windows-decision-failed")?;
                self.windows.exported = false;
            }
            ReviewWindowsCategory {
                source_id,
                category: value,
                decision,
                reason,
            } => {
                let category = categories()
                    .into_iter()
                    .find(|category| category_id(*category) == value)
                    .ok_or("installer-preview-invalid-choice")?;
                let decision = match decision.as_str() {
                    "copy" => Decision::Copy,
                    "retain-original" => Decision::RetainOriginal,
                    "exclude" => Decision::Exclude,
                    _ => return Err("installer-preview-invalid-choice"),
                };
                self.belongings
                    .windows
                    .as_mut()
                    .ok_or("installer-windows-source-failed")?
                    .select_category(&source_id, category, decision, reason)
                    .map_err(|_| "installer-windows-decision-failed")?;
                self.windows.exported = false;
            }
            SetWindowsExclusionReason { value } => self.windows.exclusion_reason.set(value),
            SetWindowsPreparationPassword { value } => self.windows.password.set(value),
            ChooseWindowsExport => {
                self.windows.picker = Some((
                    None,
                    ui::PickRequest::directory(
                        &t("installer-windows-export-title"),
                        &t("installer-windows-export"),
                    )
                    .send(),
                ));
            }
            ExportWindowsPreparation { destination } => {
                let inventory = self
                    .belongings
                    .windows
                    .as_ref()
                    .ok_or("installer-windows-source-failed")?
                    .clone();
                let password = zeroize::Zeroizing::new(self.windows.password.get());
                if password.len() < 8 {
                    return Err("installer-windows-password-required");
                }
                let hints = self.windows.hints.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                self.windows.worker = Some(rx);
                self.windows.exported = false;
                self.windows.prepared_media = None;
                self.windows.failed = false;
                std::thread::spawn(move || {
                    let _ = tx.send(export(
                        &inventory,
                        &hints,
                        password.as_bytes(),
                        Path::new(&destination),
                    ));
                });
            }
            PrepareUsb => {
                // Only the exact completed export can enter the writer's private
                // request. Target admission additionally forbids flashing its storage.
                if self.windows.media_handoff().is_none() {
                    return Err("installer-windows-source-failed");
                }
                self.media
                    .as_ref()
                    .ok_or("installer-preview-storage-unavailable")?
                    .set(true);
            }
            _ => return Err("installer-preview-wrong-page"),
        }
        self.dirty = true;
        Ok(())
    }

    pub(super) fn poll_windows(&mut self) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.windows.source_worker {
            let result = match worker.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(_) => Some(Err("Windows source review stopped".into())),
            };
            if let Some(result) = result {
                self.windows.source_worker = None;
                match result {
                    Ok(Discovery::Suggestions(suggestions)) => {
                        self.windows.suggestions = suggestions
                    }
                    Ok(Discovery::Granted {
                        selected,
                        belongings,
                        hints,
                        hints_unavailable,
                    }) => {
                        self.windows.selected = selected;
                        self.windows.hints = hints;
                        self.windows.hints_unavailable = hints_unavailable;
                        self.belongings = belongings;
                    }
                    Err(_) => self.error = Some("installer-windows-source-failed"),
                }
                changed = true;
            }
        }
        if let Some((_, picker)) = &self.windows.picker {
            let answer = match picker.try_recv() {
                Ok(answer) => Some(answer),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(_) => Some(ui::PortalOutcome::Failed(String::new())),
            };
            if let Some(answer) = answer {
                let (role, _) = self.windows.picker.take().unwrap();
                if let ui::PortalOutcome::Failed(_) = &answer {
                    self.error = Some("installer-windows-source-failed");
                }
                if let ui::PortalOutcome::Opened(path) = answer {
                    let effect = if let Some(role) = role {
                        SelectWindowsRoot {
                            source: path.to_string_lossy().into_owned(),
                            role: role_id(role).into(),
                        }
                    } else {
                        ExportWindowsPreparation {
                            destination: path.to_string_lossy().into_owned(),
                        }
                    };
                    if let Err(error) = self.apply_windows(effect) {
                        self.error = Some(error);
                    }
                }
                changed = true;
            }
        }
        if let Some(worker) = &self.windows.worker {
            match worker.try_recv() {
                Ok(result) => {
                    #[cfg(test)]
                    if let Err(error) = &result {
                        eprintln!("Windows export failure: {error}");
                    }
                    self.windows.worker = None;
                    self.windows.exported = result.is_ok();
                    self.windows.failed = result.is_err();
                    self.windows.prepared_media = result.ok();
                    self.windows.password.set(String::new());
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.windows.worker = None;
                    self.windows.failed = true;
                    changed = true;
                }
                Err(_) => {}
            }
        }
        changed
    }

    pub(super) fn windows_view(&self) -> Box<dyn Widget> {
        let mut content = page::stack()
            .push(Box::new(
                Text::new(&t("installer-windows-heading")).with_role(TextRole::Heading),
            ))
            .push(Box::new(Text::new(&t("installer-windows-intro"))))
            .push(Box::new(self.button(
                "windows.find",
                "installer-windows-find",
                InstallerPreviewEffect::FindWindowsRoots,
            )));
        for (index, suggestion) in self.windows.suggestions.iter().enumerate() {
            let commands = self.commands.clone();
            let effect = InstallerPreviewEffect::SelectWindowsRoot {
                source: suggestion.path.to_string_lossy().into_owned(),
                role: role_id(suggestion.role).into(),
            };
            content = content.push(Box::new(
                Button::new(
                    &format!("windows.suggestion.{index}"),
                    &suggestion.path.to_string_lossy(),
                )
                .on_effect(move || commands.borrow_mut().push(effect.clone())),
            ));
        }
        for value in [
            "known-folder",
            "onedrive",
            "outlook",
            "steam",
            "steam-library",
            "other",
        ] {
            content = content.push(Box::new(self.button(
                &format!("windows.choose.{value}"),
                &format!("installer-windows-choose-{value}"),
                InstallerPreviewEffect::ChooseWindowsRoot { role: value.into() },
            )));
        }
        if let Some(inventory) = &self.belongings.windows {
            let budget = inventory.budget();
            content = content.push(Box::new(Text::new(&t_args(
                "installer-windows-budget",
                &[
                    ("copy", ui::format::bytes(budget.copy_bytes).into()),
                    ("retain", ui::format::bytes(budget.retained_bytes).into()),
                    (
                        "excluded",
                        ui::format::bytes(budget.explicitly_excluded_bytes).into(),
                    ),
                    (
                        "unavailable",
                        ui::format::bytes(budget.unavailable_bytes).into(),
                    ),
                ],
            ))));
            if !budget.coverage_complete {
                content = content.push(Box::new(Text::new(&t("installer-windows-incomplete"))));
            }
            for (index, root) in inventory.roots.iter().enumerate() {
                content = content.push(Box::new(Text::new(&root.path.to_string_lossy())));
                for (suggestion_index, path) in root.additional_grants.iter().enumerate() {
                    let commands = self.commands.clone();
                    let source = path.to_string_lossy().into_owned();
                    content = content.push(Box::new(
                        Button::new(
                            &format!("windows.library.{index}.{suggestion_index}"),
                            &t_args(
                                "installer-windows-grant-library",
                                &[("path", source.clone().into())],
                            ),
                        )
                        .on_effect(move || {
                            commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::SelectWindowsRoot {
                                    source: source.clone(),
                                    role: "steam-library".into(),
                                })
                        }),
                    ));
                }
                // Categories are actual inventory rows, not an assumed app label.
                for category in categories() {
                    let count = root
                        .entries
                        .iter()
                        .filter(|entry| entry.category == category)
                        .count();
                    if count == 0 {
                        continue;
                    }
                    let category_id = category_id(category);
                    let decision = root
                        .category_decisions
                        .get(&category)
                        .copied()
                        .unwrap_or(root.decision);
                    let decision_id = match decision {
                        Decision::Copy => "copy",
                        Decision::RetainOriginal => "retain-original",
                        Decision::Exclude => "exclude",
                    };
                    content = content.push(Box::new(Text::new(&t_args(
                        "installer-windows-category-summary",
                        &[
                            (
                                "category",
                                t(&format!("installer-windows-category-{category_id}")).into(),
                            ),
                            ("count", count.to_string().into()),
                            (
                                "decision",
                                t(&format!("installer-windows-{decision_id}")).into(),
                            ),
                        ],
                    ))));
                    for decision in ["copy", "retain-original", "exclude"] {
                        content = content.push(Box::new(
                            self.button(
                                &format!("windows.root.{index}.{category_id}.{decision}"),
                                &format!("installer-windows-{decision}"),
                                InstallerPreviewEffect::ReviewWindowsCategory {
                                    source_id: root.source_id.clone(),
                                    category: category_id.into(),
                                    decision: decision.into(),
                                    reason: (decision == "exclude")
                                        .then(|| self.windows.exclusion_reason.get()),
                                },
                            ),
                        ));
                    }
                }
                for decision in ["copy", "retain-original"] {
                    content = content.push(Box::new(self.button(
                        &format!("windows.root.{index}.{decision}"),
                        &format!("installer-windows-{decision}"),
                        InstallerPreviewEffect::ReviewWindowsRoot {
                            source_id: root.source_id.clone(),
                            decision: decision.into(),
                            reason: None,
                        },
                    )));
                }
            }
            let commands = self.commands.clone();
            content = content.push(Box::new(Field::new(
                t("installer-windows-exclusion-reason"),
                TextInput::from_state(&self.windows.exclusion_reason)
                    .with_id("windows.exclusion-reason")
                    .on_change(move |value| {
                        commands.borrow_mut().push(
                            InstallerPreviewEffect::SetWindowsExclusionReason {
                                value: value.to_string(),
                            },
                        )
                    }),
            )));
            let commands = self.commands.clone();
            content = content.push(Box::new(Field::new(
                t("installer-windows-password"),
                TextInput::from_state(&self.windows.password)
                    .with_kind(TextInputKind::Password)
                    .with_secret(true)
                    .with_id("windows.password")
                    .on_change(move |value| {
                        commands.borrow_mut().push(
                            InstallerPreviewEffect::SetWindowsPreparationPassword {
                                value: value.to_string(),
                            },
                        )
                    }),
            )));
            content = content.push(Box::new(self.button(
                "windows.export",
                "installer-windows-export",
                InstallerPreviewEffect::ChooseWindowsExport,
            )));
        }
        for (show, key) in [
            (self.windows.worker.is_some(), "installer-windows-exporting"),
            (
                self.windows.source_worker.is_some(),
                "installer-windows-checking",
            ),
            (
                self.windows.hints_unavailable,
                "installer-windows-hints-unavailable",
            ),
            (self.windows.exported, "installer-windows-exported"),
            (self.windows.failed, "installer-windows-export-failed"),
        ] {
            if show {
                content = content.push(Box::new(Text::new(&t(key))));
            }
        }
        if let Some(error) = self.error {
            content = content.push(Box::new(Text::new(&t(error))));
        }
        content = content.push(Box::new(self.button(
            "windows.media",
            "installer-windows-media",
            InstallerPreviewEffect::PrepareUsb,
        )));
        content = content.push(Box::new(self.recovery_content()));
        page::Frame::new(content).build()
    }
}

fn categories() -> [crate::setup::windows_inventory::Category; 9] {
    use crate::setup::windows_inventory::Category::{
        GameInstallation, GameMod, GameSave, MailArchive, MailCacheOrDrafts, PersonalFiles,
        PotentiallyUnique, SteamUserdata, SyncData,
    };
    [
        PersonalFiles,
        SyncData,
        MailArchive,
        MailCacheOrDrafts,
        SteamUserdata,
        GameSave,
        GameMod,
        GameInstallation,
        PotentiallyUnique,
    ]
}

fn category_id(category: crate::setup::windows_inventory::Category) -> &'static str {
    use crate::setup::windows_inventory::Category::{
        GameInstallation, GameMod, GameSave, MailArchive, MailCacheOrDrafts, PersonalFiles,
        PotentiallyUnique, SteamUserdata, SyncData,
    };
    match category {
        PersonalFiles => "personal-files",
        SyncData => "sync-data",
        MailArchive => "mail-archive",
        MailCacheOrDrafts => "mail-cache-or-drafts",
        SteamUserdata => "steam-userdata",
        GameSave => "game-save",
        GameMod => "game-mod",
        GameInstallation => "game-installation",
        PotentiallyUnique => "potentially-unique",
    }
}

fn role_id(role: RootRole) -> &'static str {
    match role {
        RootRole::KnownFolder => "known-folder",
        RootRole::OneDrive => "onedrive",
        RootRole::Outlook => "outlook",
        RootRole::Steam => "steam",
        RootRole::SteamLibrary => "steam-library",
        RootRole::Other => "other",
    }
}

fn export(
    inventory: &Inventory,
    hints: &[account_custody::hints::AccountHint],
    password: &[u8],
    destination: &Path,
) -> Result<crate::windows_media::WindowsMedia, String> {
    use crate::setup::migration::PinnedDirectory;
    use std::io::Write;
    let parent = PinnedDirectory::open_absolute(destination).map_err(|e| e.to_string())?;
    inventory.validate_export_destination(&parent)?;
    let name = format!("tilo-windows-{}", uuid::Uuid::new_v4());
    let output = parent
        .create_child(std::ffi::OsStr::new(&name))
        .map_err(|e| e.to_string())?;
    let binding = uuid::Uuid::new_v4().to_string();
    let sealed = account_custody::hints::seal_password(password, &binding, hints)
        .map_err(|e| e.to_string())?;
    let dir = cap_std::fs::Dir::from_std_file(output.file.try_clone().map_err(|e| e.to_string())?);
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = dir
        .open_with("account-hints.sealed", &options)
        .map_err(|e| e.to_string())?;
    file.write_all(sealed.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    let mut verified_bytes = 0_u64;
    let mut verified_files = 0_usize;
    for (index, root) in inventory.roots.iter().enumerate() {
        if !root.has_copy_selection() {
            continue;
        }
        let report = inventory.copy_selected(
            &root.source_id,
            &destination.join(&name).join(format!("source-{index}")),
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        if !report.complete() {
            return Err("Selected data was not completely copied; verified output retained".into());
        }
        verified_bytes = verified_bytes.saturating_add(report.verified_bytes);
        verified_files = verified_files.saturating_add(report.verified_files.len());
    }
    // No source environment, account identity, source path or filename inventory
    // is written to the public receipt. It never authorizes replacement.
    let receipt = serde_json::to_vec(&serde_json::json!({"schema":1,"hint_binding":binding,
        "budget":inventory.budget(), "verified_bytes":verified_bytes, "verified_files":verified_files,
        "replacement_authorized":false}))
    .map_err(|e| e.to_string())?;
    let mut file = dir
        .open_with("review.json", &options)
        .map_err(|e| e.to_string())?;
    file.write_all(&receipt)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    output.validate_destination().map_err(|e| e.to_string())?;
    crate::setup::migration_durability::sync_directory(&output.file).map_err(|e| e.to_string())?;
    crate::setup::migration_durability::sync_directory(&parent.file).map_err(|e| e.to_string())?;
    let prepared =
        account_custody::hints::media::Prepared::seal(password, &binding, hints, &receipt)
            .map_err(|e| e.to_string())?;
    crate::windows_media::WindowsMedia::new(prepared, destination.join(name).join("review.json"))
        .map_err(|e| e.to_string())
}

use InstallerPreviewEffect::{ExportWindowsPreparation, SelectWindowsRoot};

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use InstallerPreviewEffect::*;

    #[test]
    fn windows_route_grants_reviews_and_exports_without_sign_in() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("selected");
        let backup = fixture.path().join("backup");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(root.join("archive.pst"), b"mail").unwrap();
        std::fs::write(root.join("save.dat"), b"unique").unwrap();
        let mut preview = InstallerPreview::default();
        preview.windows.enabled = true;
        preview.live = true;
        preview.media = Some(ui::LocalState::new(false));
        preview
            .apply(SelectWindowsRoot {
                source: root.to_str().unwrap().into(),
                role: "outlook".into(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while preview.windows.source_worker.is_some() && std::time::Instant::now() < deadline {
            preview.poll_windows();
            std::thread::yield_now();
        }
        assert!(!preview.windows.busy(), "source inventory must finish");
        let id = preview.belongings.windows.as_ref().unwrap().roots[0]
            .source_id
            .clone();
        assert_eq!(
            preview
                .belongings
                .windows
                .as_ref()
                .unwrap()
                .budget()
                .copy_bytes,
            0
        );
        preview
            .apply(ReviewWindowsCategory {
                source_id: id.clone(),
                category: "mail-archive".into(),
                decision: "copy".into(),
                reason: None,
            })
            .unwrap();
        assert!(
            preview
                .apply(ReviewWindowsCategory {
                    source_id: id,
                    category: "potentially-unique".into(),
                    decision: "exclude".into(),
                    reason: None
                })
                .is_err()
        );
        let budget = preview.belongings.windows.as_ref().unwrap().budget();
        assert_eq!(budget.copy_bytes, 4);
        assert_eq!(budget.retained_bytes, 6);
        let view = preview.windows_view();
        assert!(super::super::visual_tests::control(view.as_ref(), "windows.export").is_some());
        assert!(
            super::super::visual_tests::control(view.as_ref(), "windows.root.0.mail-archive.copy")
                .is_some()
        );
        assert!(preview.effect_page_admitted("select-windows-root"));
        assert!(!preview.effect_page_admitted("create-prepared-identity"));
        assert!(
            preview
                .apply(ExportWindowsPreparation {
                    destination: backup.to_str().unwrap().into()
                })
                .is_err()
        );
        preview
            .apply(SetWindowsPreparationPassword {
                value: "private-export-password".into(),
            })
            .unwrap();
        preview
            .apply(ExportWindowsPreparation {
                destination: backup.to_str().unwrap().into(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while preview.windows.worker.is_some() && std::time::Instant::now() < deadline {
            preview.poll_windows();
            std::thread::yield_now();
        }
        assert!(
            preview.windows.exported,
            "export must complete, not time out or report a partial copy"
        );
        let output = std::fs::read_dir(&backup)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read(output.join("source-0/archive.pst")).unwrap(),
            b"mail"
        );
        assert!(!output.join("source-0/save.dat").exists());
        assert_eq!(std::fs::read(root.join("save.dat")).unwrap(), b"unique");
        assert!(preview.personal.prepared_handle().is_none());
        preview.apply(PrepareUsb).unwrap();
        assert!(preview.media.as_ref().unwrap().get());
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("review.json")).unwrap()).unwrap();
        assert_eq!(receipt["replacement_authorized"], false);
        assert!(!receipt.to_string().contains(root.to_str().unwrap()));
        let encrypted = account_custody::hints::PasswordHints::from_bytes(
            &std::fs::read(output.join("account-hints.sealed")).unwrap(),
        )
        .unwrap();
        assert!(
            account_custody::hints::open_password(
                b"private-export-password",
                receipt["hint_binding"].as_str().unwrap(),
                &encrypted
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn windows_export_keeps_registry_account_identities_out_of_public_media_json() {
        use crate::setup::account_transfer::{WindowsProfile, private_windows_hints};
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let selected = [SelectedRoot {
            path: source.path().to_owned(),
            role: RootRole::OneDrive,
        }];
        let profiles = [WindowsProfile {
            root: source.path().to_owned(),
            email: "private-export-fixture@example.org".into(),
        }];
        let hints = private_windows_hints(&selected, &profiles);
        let inventory = Inventory::discover(&selected).unwrap();
        export(&inventory, &hints, b"private-password", destination.path()).unwrap();
        let root = std::fs::read_dir(destination.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let public = std::fs::read_to_string(root.join("review.json")).unwrap();
        assert!(!public.contains(&profiles[0].email));
        assert!(!public.contains(source.path().to_str().unwrap()));
        let receipt: serde_json::Value = serde_json::from_str(&public).unwrap();
        let bytes = std::fs::read(root.join("account-hints.sealed")).unwrap();
        assert!(
            !bytes
                .windows(profiles[0].email.len())
                .any(|bytes| bytes == profiles[0].email.as_bytes())
        );
        let sealed = account_custody::hints::PasswordHints::from_bytes(&bytes).unwrap();
        let opened = account_custody::hints::open_password(
            b"private-password",
            receipt["hint_binding"].as_str().unwrap(),
            &sealed,
        )
        .unwrap();
        assert_eq!(opened[0].email, profiles[0].email);
    }

    #[test]
    fn windows_export_cannot_modify_a_retained_source() {
        let fixture = tempfile::tempdir().unwrap();
        let inventory = Inventory::discover(&[SelectedRoot {
            path: fixture.path().to_owned(),
            role: RootRole::Other,
        }])
        .unwrap();
        assert!(export(&inventory, &[], b"private-password", fixture.path()).is_err());
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
    }
}
