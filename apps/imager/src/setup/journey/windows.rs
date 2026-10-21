//! Windows prepares selected local data and private sign-in suggestions only.
//! Identity enrollment and account authorization happen later in the live OS.
use super::*;
use crate::setup::windows_inventory::{Decision, RootRole, SelectedRoot};
use std::path::Path;

enum Discovery {
    Suggestions(Vec<crate::setup::windows_inventory::RootSuggestion>),
    Granted {
        selected: Vec<SelectedRoot>,
        belongings: belongings::Belongings,
        hints: Vec<imager_backend::windows_preparation::AccountHint>,
        hints_unavailable: bool,
    },
}

pub(super) struct Preparation {
    pub enabled: bool,
    selected: Vec<SelectedRoot>,
    suggestions: Vec<crate::setup::windows_inventory::RootSuggestion>,
    hints: Vec<imager_backend::windows_preparation::AccountHint>,
    password: ui::LocalState<String>,
    exclusion_reason: ui::LocalState<String>,
    hints_unavailable: bool,
    picker: Option<(Option<RootRole>, ui::PortalChannel)>,
    worker: Option<std::sync::mpsc::Receiver<Result<crate::windows_media::WindowsMedia, String>>>,
    prepared_media: Option<crate::windows_media::WindowsMedia>,
    source_worker: Option<std::sync::mpsc::Receiver<Result<Discovery, String>>>,
    exported: bool,
    failed: bool,
    expanded: std::collections::BTreeSet<(String, String)>,
    opener: fn(&str) -> ui::portal::LaunchChannel,
    opening: Option<ui::portal::LaunchChannel>,
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
            expanded: std::collections::BTreeSet::new(),
            opener: ui::portal::send_open_uri,
            opening: None,
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
            "sign_in": "live_only", "replacement_authorized":false,
            "opening_source": self.opening.is_some(), "expanded":self.expanded})
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
    fn windows_view_button(&self, id: &str, label: &str, effect: InstallerPreviewEffect) -> Button {
        let commands = self.commands.clone();
        Button::new(id, &t(label)).on_view(move || commands.borrow_mut().push(effect.clone()))
    }

    pub(super) fn windows_review_projection(&self) -> serde_json::Value {
        let Some(inventory) = &self.belongings.windows else {
            return serde_json::json!([]);
        };
        serde_json::json!(inventory.roots.iter().enumerate().map(|(index, root)| {
            serde_json::json!({"source_id":root.source_id, "source_path":root.path,
                "categories":super::super::windows_inventory::review_categories(root, index),
                "gaps":root.gaps.iter().map(|gap| root.path.join(&gap.relative)).collect::<Vec<_>>()})
        }).collect::<Vec<_>>())
    }

    pub(super) fn apply_windows(
        &mut self,
        effect: InstallerPreviewEffect,
    ) -> Result<(), &'static str> {
        use InstallerPreviewEffect::{
            ChooseWindowsExport, ChooseWindowsRoot, ExportWindowsPreparation, FindWindowsRoots,
            OpenWindowsSource, PrepareUsb, ReadPreview, ReviewWindowsCategory, ReviewWindowsRoot,
            SelectWindowsRoot, SetWindowsExclusionReason, SetWindowsPreparationPassword,
            ShowWindowsCategory,
        };
        if self.windows.busy() && !matches!(effect, ReadPreview) {
            return Err("installer-preview-transfer-busy");
        }
        match effect {
            ReadPreview => {}
            InstallerPreviewEffect::ChoosePreview { choice }
                if self.journey.page == Page::Destination =>
            {
                self.journey.destination = Some(match choice.as_str() {
                    "computer" => Destination::Computer,
                    "another" => Destination::Another,
                    _ => return Err("installer-preview-invalid-choice"),
                });
            }
            InstallerPreviewEffect::ContinuePreview
                if self.journey.page == Page::Destination
                    && self.journey.destination == Some(Destination::Computer) =>
            {
                self.native_full_os_review_pending = true;
            }
            InstallerPreviewEffect::BackInPreview => {
                self.journey.destination = None;
                self.journey.page = Page::Destination;
            }
            OpenWindowsSource { source_id } => {
                if self.windows.opening.is_some() {
                    return Err("installer-windows-open-pending");
                }
                let uri = self
                    .belongings
                    .windows
                    .as_ref()
                    .ok_or("installer-windows-source-failed")?
                    .source_folder_uri(&source_id)
                    .map_err(|_| "installer-windows-source-failed")?;
                self.windows.opening = Some((self.windows.opener)(&uri));
            }
            ShowWindowsCategory {
                source_id,
                category,
                expanded,
            } => {
                let present = self.belongings.windows.as_ref().is_some_and(|inventory| {
                    inventory.roots.iter().any(|root| {
                        root.source_id == source_id
                            && ((category == "gaps" && !root.gaps.is_empty())
                                || root
                                    .entries
                                    .iter()
                                    .any(|entry| category_id(entry.category) == category))
                    })
                });
                if !present {
                    return Err("installer-preview-invalid-choice");
                }
                let key = (source_id, category);
                if expanded {
                    self.windows.expanded.insert(key);
                } else {
                    self.windows.expanded.remove(&key);
                }
            }
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
        if let Some(opening) = &self.windows.opening {
            let result = match opening.try_recv() {
                Ok(ui::portal::LaunchOutcome::Opened) => Some(true),
                Ok(ui::portal::LaunchOutcome::Failed(_))
                | Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(false),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            };
            if let Some(success) = result {
                self.windows.opening = None;
                if !success {
                    self.error = Some("installer-windows-open-failed");
                }
                changed = true;
            }
        }
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
                        mut belongings,
                        hints,
                        hints_unavailable,
                    }) => {
                        // Preserve local bytes by default, including unknown game
                        // folders and mods. Unavailable bytes are never claimed copied.
                        if let Some(inventory) = &mut belongings.windows {
                            for root in &mut inventory.roots {
                                if let Some(previous) =
                                    self.belongings.windows.as_ref().and_then(|old| {
                                        old.roots.iter().find(|old| old.source_id == root.source_id)
                                    })
                                {
                                    root.decision = previous.decision;
                                    root.exclusion_reason.clone_from(&previous.exclusion_reason);
                                    root.category_decisions
                                        .clone_from(&previous.category_decisions);
                                    root.category_exclusion_reasons
                                        .clone_from(&previous.category_exclusion_reasons);
                                } else {
                                    root.decision = Decision::Copy;
                                }
                            }
                        }
                        self.windows.expanded.clear();
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

    pub(super) fn windows_destination_view(&self) -> Box<dyn Widget> {
        let mut content = page::stack()
            .push(Box::new(
                Text::new(&t("installer-preview-destination-heading")).with_role(TextRole::Heading),
            ))
            .push(Box::new(self.choice(
                "computer",
                self.journey.destination == Some(Destination::Computer),
            )))
            .push(Box::new(self.choice("another", false)));
        if let Some(error) = self.error {
            content = content.push(Box::new(Text::new(&t(error))));
        }
        content = content.push(Box::new(
            self.button(
                "installer.continue",
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            )
            .with_enabled(self.journey.destination == Some(Destination::Computer)),
        ));
        page::Frame::new(content).with_cursor(self.cursor).build()
    }

    pub(super) fn windows_view(&self) -> Box<dyn Widget> {
        let mut content = page::stack()
            .push(Box::new(self.button(
                "installer.back",
                "installer-preview-back",
                InstallerPreviewEffect::BackInPreview,
            )))
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
                    ("copy", ui::format::memory(budget.copy_bytes).into()),
                    ("retain", ui::format::memory(budget.retained_bytes).into()),
                    (
                        "excluded",
                        ui::format::memory(budget.explicitly_excluded_bytes).into(),
                    ),
                    (
                        "unavailable",
                        ui::format::memory(budget.unavailable_bytes).into(),
                    ),
                ],
            ))));
            content = content.push(Box::new(Text::new(&t("installer-windows-backup-plan"))));
            if !budget.coverage_complete {
                content = content.push(Box::new(Text::new(&t("installer-windows-incomplete"))));
            }
            for (index, root) in inventory.roots.iter().enumerate() {
                content = content.push(Box::new(Text::new(&root.path.to_string_lossy())));
                content = content.push(Box::new(
                    self.windows_view_button(
                        &format!("windows.root.{index}.open"),
                        "installer-windows-open-source",
                        InstallerPreviewEffect::OpenWindowsSource {
                            source_id: root.source_id.clone(),
                        },
                    )
                    .with_enabled(self.windows.opening.is_none()),
                ));
                if !root.gaps.is_empty() {
                    content = content.push(Box::new(Text::new(&t_args(
                        "installer-windows-gaps",
                        &[("count", root.gaps.len().to_string().into())],
                    ))));
                    let expanded = self
                        .windows
                        .expanded
                        .contains(&(root.source_id.clone(), "gaps".into()));
                    content = content.push(Box::new(self.windows_view_button(
                        &format!("windows.root.{index}.gaps"),
                        if expanded {
                            "installer-windows-hide-paths"
                        } else {
                            "installer-windows-show-paths"
                        },
                        InstallerPreviewEffect::ShowWindowsCategory {
                            source_id: root.source_id.clone(),
                            category: "gaps".into(),
                            expanded: !expanded,
                        },
                    )));
                    if expanded {
                        for gap in &root.gaps {
                            content = content.push(Box::new(Text::new(&t_args(
                                "installer-windows-gap-path",
                                &[(
                                    "path",
                                    root.path
                                        .join(&gap.relative)
                                        .to_string_lossy()
                                        .into_owned()
                                        .into(),
                                )],
                            ))));
                        }
                    }
                }
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
                if root.entries.is_empty() && root.gaps.is_empty() {
                    content = content.push(Box::new(Text::new(&t("installer-windows-empty"))));
                }
                for group in super::super::windows_inventory::review_categories(root, index) {
                    let category_id = category_id(group.category);
                    let mut items: Vec<Box<dyn Widget>> = Vec::new();
                    let decision_id = match group.decision {
                        Decision::Copy => "copy",
                        Decision::RetainOriginal => "retain-original",
                        Decision::Exclude => "exclude",
                    };
                    items.push(Box::new(Text::new(&t_args(
                        "installer-windows-category-summary",
                        &[
                            (
                                "category",
                                t(&format!("installer-windows-category-{category_id}")).into(),
                            ),
                            ("count", group.entries.to_string().into()),
                            (
                                "decision",
                                t(&format!("installer-windows-{decision_id}")).into(),
                            ),
                            ("size", ui::format::memory(group.known_bytes).into()),
                            ("unknown", group.unknown_sizes.to_string().into()),
                            ("unavailable", group.unavailable.to_string().into()),
                        ],
                    ))));
                    items.push(Box::new(Text::new(&t_args(
                        &format!("installer-windows-plan-{decision_id}"),
                        &[("destination", group.destination.clone().into())],
                    ))));
                    let expanded = self
                        .windows
                        .expanded
                        .contains(&(root.source_id.clone(), category_id.into()));
                    items.push(Box::new(self.windows_view_button(
                        &format!("windows.root.{index}.{category_id}.details"),
                        if expanded {
                            "installer-windows-hide-paths"
                        } else {
                            "installer-windows-show-paths"
                        },
                        InstallerPreviewEffect::ShowWindowsCategory {
                            source_id: root.source_id.clone(),
                            category: category_id.into(),
                            expanded: !expanded,
                        },
                    )));
                    if expanded {
                        for entry in &group.paths {
                            let availability = if entry.copy_eligible {
                                "local"
                            } else {
                                match entry.availability {
                                    crate::setup::windows_inventory::Availability::Placeholder => {
                                        "placeholder"
                                    }
                                    _ => "unsupported",
                                }
                            };
                            items.push(Box::new(Text::new(&t_args(
                                "installer-windows-path",
                                &[
                                    ("path", entry.path.clone().into()),
                                    (
                                        "size",
                                        entry
                                            .bytes
                                            .map_or_else(
                                                || t("installer-windows-size-unknown"),
                                                ui::format::memory,
                                            )
                                            .into(),
                                    ),
                                    (
                                        "availability",
                                        t(&format!(
                                            "installer-windows-availability-{availability}"
                                        ))
                                        .into(),
                                    ),
                                ],
                            ))));
                        }
                    }
                    for decision in ["copy", "retain-original", "exclude"] {
                        items.push(Box::new(
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
                    content = content.push(Box::new(ui::widget::Section::titled(
                        t(&format!("installer-windows-category-{category_id}")),
                        items,
                    )));
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
        page::Frame::new(content).with_cursor(self.cursor).build()
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

use InstallerPreviewEffect::{ExportWindowsPreparation, SelectWindowsRoot};
use imager_backend::windows_preparation::export;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::setup::windows_inventory::Inventory;
    use InstallerPreviewEffect::*;

    thread_local! {
        static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn recording_opener(uri: &str) -> ui::portal::LaunchChannel {
        OPENED.with(|opened| opened.borrow_mut().push(uri.into()));
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(ui::portal::LaunchOutcome::Opened).unwrap();
        rx
    }

    fn fixture_preview(root: &Path, role: RootRole) -> InstallerPreview {
        let mut preview = InstallerPreview::default();
        preview.windows.enabled = true;
        preview.live = true;
        preview.windows.opener = recording_opener;
        preview.belongings = belongings::Belongings::from_windows_roots(&[SelectedRoot {
            path: root.into(),
            role,
        }])
        .unwrap();
        preview
    }

    fn texts(widget: &dyn Widget) -> Vec<String> {
        let mut result = widget
            .accessibility()
            .and_then(|node| node.name)
            .into_iter()
            .collect::<Vec<_>>();
        for child in widget.children() {
            result.extend(texts(child));
        }
        result
    }

    #[test]
    fn windows_review_projects_real_paths_and_progressively_discloses_them() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Documents & local mail");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("archive.pst"), b"mail").unwrap();
        std::fs::create_dir(root.join("saves")).unwrap();
        std::fs::write(root.join("saves/game.dat"), b"save").unwrap();
        std::fs::create_dir(root.join("mods")).unwrap();
        std::fs::write(root.join("mods/unknown.bin"), b"mod").unwrap();
        let mut preview = fixture_preview(&root, RootRole::Other);
        let id = preview.belongings.windows.as_ref().unwrap().roots[0]
            .source_id
            .clone();
        let projection = preview.windows_review_projection();
        let groups = projection[0]["categories"].as_array().unwrap();
        assert_eq!(groups.len(), 3);
        let mail = groups
            .iter()
            .find(|group| group["category"] == "mail-archive")
            .unwrap();
        assert_eq!(mail["known_bytes"], 4);
        assert_eq!(
            mail["paths"][0]["path"],
            root.join("archive.pst").to_str().unwrap()
        );
        assert_eq!(mail["paths"][0]["destination"], "source-0/archive.pst");
        assert_eq!(mail["unavailable"], 0);
        let before = texts(preview.windows_view().as_ref());
        assert!(
            !before
                .iter()
                .any(|label| label.contains(root.join("archive.pst").to_str().unwrap()))
        );
        preview
            .apply(ShowWindowsCategory {
                source_id: id.clone(),
                category: "mail-archive".into(),
                expanded: true,
            })
            .unwrap();
        let after = texts(preview.windows_view().as_ref());
        assert!(
            after
                .iter()
                .any(|label| label.contains(mail["paths"][0]["path"].as_str().unwrap()))
        );
        let view = preview.windows_view();
        let open =
            super::super::visual_tests::control(view.as_ref(), "windows.root.0.open").unwrap();
        assert_eq!(
            open.accessibility().unwrap().name.as_deref(),
            Some(t("installer-windows-open-source").as_str())
        );
        let resources = ui::widget::published_resources(open, &automerge::AutoCommit::new());
        assert_eq!(
            resources[0].actions[0].kind,
            ui::tilo_actions::OperationKind::View
        );
        let declarations = InstallerPreviewEffect::declarations();
        for id in ["open-windows-source", "show-windows-category"] {
            assert!(preview.effect_page_admitted(id));
            assert_eq!(
                declarations
                    .iter()
                    .find(|declaration| declaration.id == id)
                    .unwrap()
                    .kind,
                ui::tilo_actions::OperationKind::View
            );
            if id == "open-windows-source" {
                let declaration = declarations
                    .iter()
                    .find(|declaration| declaration.id == id)
                    .unwrap();
                assert_eq!(declaration.params.len(), 1);
                assert_eq!(declaration.params[0].name, "source_id");
            }
        }
        assert!(
            preview
                .apply(ShowWindowsCategory {
                    source_id: id,
                    category: "absent".into(),
                    expanded: true,
                })
                .is_err()
        );
    }

    #[test]
    fn windows_review_exposes_unavailable_entries_and_gaps_without_following_them() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("granted");
        let outside = fixture.path().join("not-granted");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("unique.txt"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked-folder")).unwrap();
        let mut preview = fixture_preview(&root, RootRole::Other);
        let projected = preview.windows_review_projection();
        let group = &projected[0]["categories"][0];
        assert_eq!(group["entries"], 1);
        assert_eq!(group["known_bytes"], 0);
        assert_eq!(group["unknown_sizes"], 1);
        assert_eq!(group["unavailable"], 1);
        assert!(group["paths"][0]["bytes"].is_null());
        assert!(!projected.to_string().contains("unique.txt"));
        assert_eq!(
            projected[0]["gaps"][0],
            root.join("linked-folder").to_str().unwrap()
        );
        let source_id = projected[0]["source_id"].as_str().unwrap().to_owned();
        preview
            .apply(ShowWindowsCategory {
                source_id,
                category: "gaps".into(),
                expanded: true,
            })
            .unwrap();
        assert!(
            texts(preview.windows_view().as_ref())
                .iter()
                .any(|text| text.contains(root.join("linked-folder").to_str().unwrap()))
        );
    }

    #[test]
    fn windows_open_uses_one_escaped_file_uri_and_rejects_paths_and_replaced_roots() {
        OPENED.with(|opened| opened.borrow_mut().clear());
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("User's & mail #100% é");
        std::fs::create_dir(&root).unwrap();
        let mut preview = fixture_preview(&root, RootRole::Outlook);
        // Production uses the shared send_open_uri, whose Windows native
        // backend enables ShellExecute rather than interpolating a shell command.
        assert!(std::ptr::fn_addr_eq(
            Preparation::default().opener,
            ui::portal::send_open_uri as fn(&str) -> ui::portal::LaunchChannel,
        ));
        let id = preview.belongings.windows.as_ref().unwrap().roots[0]
            .source_id
            .clone();
        assert!(
            preview
                .apply(OpenWindowsSource {
                    source_id: root.to_string_lossy().into_owned()
                })
                .is_err()
        );
        preview
            .apply(OpenWindowsSource {
                source_id: id.clone(),
            })
            .unwrap();
        assert!(
            preview
                .apply(OpenWindowsSource {
                    source_id: id.clone()
                })
                .is_err()
        );
        preview.poll_windows();
        OPENED.with(|opened| {
            let calls = opened.borrow();
            assert_eq!(calls.len(), 1);
            let uri = url::Url::parse(&calls[0]).unwrap();
            assert_eq!(uri.scheme(), "file");
            assert_eq!(uri.to_file_path().unwrap(), root);
            assert!(uri.query().is_none());
            assert!(uri.fragment().is_none());
            assert!(calls[0].contains("%23"));
            assert!(calls[0].contains("%25"));
        });
        std::fs::rename(&root, fixture.path().join("original")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(preview.apply(OpenWindowsSource { source_id: id }).is_err());
        OPENED.with(|opened| assert_eq!(opened.borrow().len(), 1));
    }

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
        let deadline = tilo_test_env::hang_deadline();
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
            10
        );
        preview
            .apply(ReviewWindowsCategory {
                source_id: id.clone(),
                category: "potentially-unique".into(),
                decision: "retain-original".into(),
                reason: None,
            })
            .unwrap();
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
        let deadline = tilo_test_env::hang_deadline();
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
