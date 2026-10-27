use super::*;

impl FileExplorerWidget {
    /// The menu item specs for the current selection / clipboard / state, given
    /// the available `templates`. Returns `(label, command)` pairs; a `None`
    /// command with an empty label is a separator. Pure (templates injected) so
    /// the menu contents — including the "New File ▸" gating — are unit-testable.
    // This declarative menu state machine intentionally keeps visibility and
    // command gating adjacent for auditability and accessibility parity.
    #[allow(
        clippy::too_many_lines,
        reason = "one declarative menu specification keeps visibility, enablement, and command identity auditable"
    )]
    pub(super) fn context_menu_spec(
        &self,
        templates: &[PathBuf],
    ) -> Vec<(String, Option<FileCommand>)> {
        let has_sel = !self.selection.is_empty();
        let can_paste = self.can_paste_clipboard();
        let has_templates = !templates.is_empty();
        let in_trash = self.is_trash_view();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();

        // Recent gathers files from many folders: it is for finding and
        // reopening, not for editing in place.
        if self.is_recent_view() {
            return self.recent_menu_spec();
        }
        if let Some(spec) = self.applications_menu_spec() {
            return spec;
        }

        // An archive is a read-only folder view. Synthetic member paths must
        // never leak into filesystem mutation commands such as rename, trash,
        // drag, paste, sharing, or terminal launch.
        if crate::archive::location(&self.current_path).is_some() {
            if has_sel {
                spec.push((
                    t("files-menu-open"),
                    self.selection.primary().map(FileCommand::Activate),
                ));
                spec.push((String::new(), None));
            }
            spec.push((
                t("files-archive-extract-here"),
                Some(FileCommand::ExtractCurrentArchive),
            ));
            spec.push((t("files-archive-extract-to"), Some(FileCommand::ExtractTo)));
            return tidy_menu(spec);
        }

        // The Trash is a restore/purge surface, not an editing one: offer Open
        // plus Restore / permanent-delete / Empty, and nothing that would edit
        // in place.
        if in_trash {
            if has_sel {
                spec.push((
                    t("files-menu-open"),
                    self.selection.primary().map(FileCommand::Activate),
                ));
                spec.push((String::new(), None));
                spec.push((t("files-menu-restore"), Some(FileCommand::RestoreFromTrash)));
                if !self
                    .selected_paths()
                    .iter()
                    .any(|path| self.is_native_trash_entry(path))
                {
                    spec.push((
                        t("files-menu-delete-permanently"),
                        Some(FileCommand::Delete),
                    ));
                }
            }
            spec.push((String::new(), None));
            if !self.has_native_trash_entries() {
                spec.push((t("files-menu-empty-trash"), Some(FileCommand::EmptyTrash)));
            }
            return tidy_menu(spec);
        }

        // A file's menu is about the file: open it, change it, organise it,
        // collaborate on it. What belongs to the folder (new items, sorting,
        // hidden files, a terminal) is the menu of the folder's empty space.
        if has_sel {
            let single = self.selection.selected().len() == 1;
            spec.push((
                t("files-menu-open"),
                self.selection.primary().map(FileCommand::Activate),
            ));
            if self.open_with_available() {
                spec.push((t("files-menu-open-with"), Some(FileCommand::OpenWithMenu)));
            }
            if single {
                if !self
                    .shortcut_menu
                    .offered_on(&self.selected_entry_identities())
                    .is_empty()
                {
                    spec.push((
                        t("files-menu-run-shortcut"),
                        Some(FileCommand::OpenShortcutsMenu),
                    ));
                }
                // Only offered while one of the person's devices is there
                // to receive.
                if self
                    .nearby_send
                    .targets()
                    .iter()
                    .any(|target| target.online)
                {
                    spec.push((
                        t("files-menu-send-nearby"),
                        Some(FileCommand::OpenNearbySend),
                    ));
                }
            }
            spec.push((String::new(), None));
            spec.push((t("files-menu-cut"), Some(FileCommand::Cut)));
            spec.push((t("files-menu-copy"), Some(FileCommand::Copy)));
            if can_paste {
                spec.push((t("files-menu-paste"), Some(FileCommand::Paste)));
            }
            spec.push((t("files-menu-rename"), Some(FileCommand::Rename)));
            spec.push((t("files-menu-move-trash"), Some(FileCommand::Delete)));
            spec.push((String::new(), None));
            let displayed = self.displayed_entries();
            if self.selection.selected().iter().any(|index| {
                displayed
                    .get(*index)
                    .is_some_and(|entry| crate::archive::is_supported(&entry.path))
            }) {
                spec.push((
                    t("files-archive-extract-here"),
                    Some(FileCommand::ExtractHere),
                ));
                spec.push((t("files-archive-extract-to"), Some(FileCommand::ExtractTo)));
            }
            spec.push((
                t("files-archive-compress-zip"),
                Some(FileCommand::CompressSelection(crate::archive::Packing::Zip)),
            ));
            spec.push((
                t("files-archive-compress-tar-zst"),
                Some(FileCommand::CompressSelection(
                    crate::archive::Packing::TarZst,
                )),
            ));
            // Favourites: add the single selected folder.
            if let Some(dir) = self.single_selected_dir() {
                spec.push((
                    t("files-menu-add-favourite"),
                    Some(FileCommand::AddFavorite(dir)),
                ));
            }
            let collaborative: Vec<_> = self
                .selection
                .selected()
                .iter()
                .filter_map(|index| displayed.get(*index))
                .filter(|entry| entry.availability.is_some())
                .collect();
            if !collaborative.is_empty() {
                let any_automatic = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        availability.policy == fs_protocol::AvailabilityPolicy::Automatic
                    })
                });
                let any_directly_pinned = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        availability.policy == fs_protocol::AvailabilityPolicy::Pinned
                            && !availability.policy_inherited
                    })
                });
                spec.push((String::new(), None));
                let any_inherited = collaborative.iter().any(|entry| {
                    entry
                        .availability
                        .as_ref()
                        .is_some_and(|availability| availability.policy_inherited)
                });
                if any_automatic {
                    spec.push((t("files-keep-offline"), Some(FileCommand::KeepOffline)));
                }
                if any_directly_pinned {
                    spec.push((
                        t("files-use-automatic-storage"),
                        Some(FileCommand::RemoveOfflinePin),
                    ));
                }
                if any_inherited && !any_directly_pinned {
                    spec.push((t("files-pinned-by-parent"), None));
                }
                let any_releasable = collaborative.iter().any(|entry| {
                    !entry.is_dir
                        && entry.availability.as_ref().is_some_and(|availability| {
                            availability.policy == fs_protocol::AvailabilityPolicy::Automatic
                                && !availability.policy_inherited
                                && availability.transfer.phase == fs_protocol::TransferPhase::Idle
                                && matches!(
                                    availability.residency,
                                    fs_protocol::Residency::Partial { .. }
                                        | fs_protocol::Residency::Resident { .. }
                                )
                        })
                });
                if any_releasable {
                    spec.push((
                        t("files-free-local-space"),
                        Some(FileCommand::FreeLocalSpace),
                    ));
                }
                let any_retryable = collaborative.iter().any(|entry| {
                    entry.availability.as_ref().is_some_and(|availability| {
                        !matches!(
                            availability.residency,
                            fs_protocol::Residency::Resident { .. }
                        ) && availability.transfer.phase == fs_protocol::TransferPhase::Idle
                    })
                });
                if any_retryable {
                    spec.push((t("files-retry-download"), Some(FileCommand::RetryTransfer)));
                }
            }
            if self.selection.selected().len() == 1
                && let Some(entry) = self
                    .selection
                    .primary()
                    .and_then(|index| displayed.get(index))
            {
                spec.push((
                    t("files-menu-properties"),
                    Some(FileCommand::ShowProperties),
                ));
                if !entry.is_dir
                    && entry.record_id.is_some()
                    && crate::model::share_control_socket(&entry.path, &self.workspace_roots)
                        .is_some()
                {
                    for human in self
                        .trusted_humans
                        .humans()
                        .iter()
                        .filter(|human| !human.devices.is_empty())
                        .take(16)
                    {
                        // Keep the concise default, then expose every named
                        // permission/expiry choice without ever placing the
                        // display label into the authority command.
                        spec.push((
                            t_args(
                                "files-share-with",
                                &[arg("person", human.display_name.clone())],
                            ),
                            Some(FileCommand::ShareWithPerson {
                                person: human.person,
                            }),
                        ));
                        for permission in [
                            fs_protocol::FilePermission::View,
                            fs_protocol::FilePermission::Edit,
                        ] {
                            for expiry in [
                                crate::sharing::ShareExpiryChoice::Day,
                                crate::sharing::ShareExpiryChoice::Week,
                                crate::sharing::ShareExpiryChoice::Month,
                                crate::sharing::ShareExpiryChoice::Never,
                            ] {
                                spec.push((
                                    t_args(
                                        "files-share-composer-named",
                                        &[
                                            arg("person", human.display_name.clone()),
                                            arg("permission", share_permission_label(permission)),
                                            arg("expiry", share_expiry_label(expiry)),
                                        ],
                                    ),
                                    Some(FileCommand::CreateShare(
                                        crate::sharing::ShareComposerRequest {
                                            subject: crate::sharing::ShareSubjectChoice::Named(
                                                human.person,
                                            ),
                                            permission,
                                            expiry,
                                            link_uses: None,
                                            recipient_devices: human
                                                .devices
                                                .iter()
                                                .map(|device| device.device)
                                                .collect(),
                                        },
                                    )),
                                ));
                            }
                        }
                    }
                    spec.push((
                        t("files-copy-one-time-link"),
                        Some(FileCommand::CreateOneTimeShareLink),
                    ));
                    for permission in [
                        fs_protocol::FilePermission::View,
                        fs_protocol::FilePermission::Edit,
                    ] {
                        for expiry in [
                            crate::sharing::ShareExpiryChoice::Day,
                            crate::sharing::ShareExpiryChoice::Week,
                            crate::sharing::ShareExpiryChoice::Month,
                        ] {
                            for link_uses in [
                                crate::sharing::LinkUseChoice::OneUse,
                                crate::sharing::LinkUseChoice::Bounded(10),
                            ] {
                                let uses = match link_uses {
                                    crate::sharing::LinkUseChoice::OneUse => {
                                        t("files-share-choice-one-use")
                                    }
                                    crate::sharing::LinkUseChoice::Bounded(count) => t_args(
                                        "files-share-choice-bounded-uses",
                                        &[arg("count", i64::from(count))],
                                    ),
                                };
                                spec.push((
                                    t_args(
                                        "files-share-composer-link",
                                        &[
                                            arg("permission", share_permission_label(permission)),
                                            arg("expiry", share_expiry_label(expiry)),
                                            arg("uses", uses),
                                        ],
                                    ),
                                    Some(FileCommand::CreateShare(
                                        crate::sharing::ShareComposerRequest {
                                            subject: crate::sharing::ShareSubjectChoice::Link,
                                            permission,
                                            expiry,
                                            link_uses: Some(link_uses),
                                            recipient_devices: Vec::new(),
                                        },
                                    )),
                                ));
                            }
                        }
                    }
                }
                if entry.share.is_some() {
                    spec.push((t("files-sharing-access"), Some(FileCommand::ShowSharing)));
                }
                if entry.availability.is_some() {
                    spec.push((
                        t("files-availability-title-short"),
                        Some(FileCommand::ShowAvailability),
                    ));
                }
                if crate::model::share_control_socket(&entry.path, &self.workspace_roots).is_some()
                    && let Some(share) = &entry.share
                {
                    for grant in share
                        .grants
                        .iter()
                        .filter(|grant| grant.status == fs_protocol::ShareGrantStatus::Active)
                    {
                        let grant_id = grant.grant_id;
                        let subject = match grant.subject {
                            fs_protocol::ShareSubjectProjection::Person { person } => {
                                self.trusted_humans.display_name_or_fingerprint(&person)
                            }
                            fs_protocol::ShareSubjectProjection::BearerLink => {
                                t("files-shared-link")
                            }
                        };
                        spec.push((
                            t_args("files-revoke-subject", &[arg("subject", subject)]),
                            Some(FileCommand::RevokeShare { grant_id }),
                        ));
                    }
                }
                if entry.history.is_some() {
                    spec.push((t("files-version-history"), Some(FileCommand::ShowHistory)));
                    if let Some(version_id) = entry.history.as_ref().and_then(|history| {
                        history.versions.iter().rev().find_map(|version| {
                            (!history.heads.contains(&version.id) && version.content_hash.is_some())
                                .then_some(version.id)
                        })
                    }) {
                        spec.push((
                            t("files-restore-previous"),
                            Some(FileCommand::RestoreVersion { version_id }),
                        ));
                    }
                    if let (Some(history), Some(current_hash)) =
                        (entry.history.as_ref(), entry.content_hash)
                    {
                        for conflict in history.open_conflicts() {
                            let members = conflict
                                .versions
                                .iter()
                                .filter_map(|id| {
                                    history.versions.iter().find(|version| version.id == *id)
                                })
                                .collect::<Vec<_>>();
                            let current = members
                                .iter()
                                .copied()
                                .filter(|version| {
                                    version.content_hash.map(|hash| hash.0) == Some(current_hash)
                                })
                                .collect::<Vec<_>>();
                            // An exact unique hash match is the strongest fact
                            // this bounded V1 projection exposes. The daemon
                            // additionally CAS-checks its signed revision, so a
                            // stale menu action fails instead of changing meaning.
                            if current.len() != 1 {
                                continue;
                            }
                            let Some(current_version) = current.first() else {
                                continue;
                            };
                            let current_id = current_version.id;
                            spec.push((String::new(), None));
                            spec.push((
                                t_args(
                                    "files-resolve-keep-current",
                                    &[arg("conflict", short_uuid(conflict.id))],
                                ),
                                Some(FileCommand::ResolveConflict {
                                    conflict_id: conflict.id,
                                    strategy: fs_protocol::ConflictStrategy::KeepCurrent,
                                    version_id: current_id,
                                }),
                            ));
                            for other in members
                                .into_iter()
                                .filter(|version| version.id != current_id)
                            {
                                let other_id = other.id;
                                spec.push((
                                    t_args(
                                        "files-resolve-use-other",
                                        &[
                                            arg("conflict", short_uuid(conflict.id)),
                                            arg("device", short_fingerprint(&other.actor.device.0)),
                                        ],
                                    ),
                                    Some(FileCommand::ResolveConflict {
                                        conflict_id: conflict.id,
                                        strategy: fs_protocol::ConflictStrategy::UseOther,
                                        version_id: other_id,
                                    }),
                                ));
                                if merge_supported_path(&entry.path) {
                                    spec.push((
                                        format!(
                                            "{} ({})",
                                            t("files-merge-review-menu"),
                                            short_fingerprint(&other.actor.device.0),
                                        ),
                                        Some(FileCommand::ReviewConflictMerge {
                                            conflict_id: conflict.id,
                                            version_id: other_id,
                                            other_device: other.actor.device.0,
                                        }),
                                    ));
                                }
                                spec.push((
                                    format!(
                                        "{} ({})",
                                        conflict_strategy_label(
                                            fs_protocol::ConflictStrategy::KeepBoth,
                                        ),
                                        short_fingerprint(&other.actor.device.0),
                                    ),
                                    Some(FileCommand::ResolveConflict {
                                        conflict_id: conflict.id,
                                        strategy: fs_protocol::ConflictStrategy::KeepBoth,
                                        version_id: other_id,
                                    }),
                                ));
                            }
                        }
                    }
                }
            }
            return tidy_menu(spec);
        }

        // The folder's own menu (nothing is selected).
        spec.push((t("files-menu-new-folder"), Some(FileCommand::NewFolder)));
        // Only offer "New file…" when the Templates dir actually has files.
        if has_templates {
            spec.push((
                t("files-menu-new-file"),
                Some(FileCommand::OpenTemplatesMenu),
            ));
        }
        if can_paste {
            spec.push((t("files-menu-paste"), Some(FileCommand::Paste)));
        }
        if self.hosted_clipboard() {
            spec.push((
                t("files-menu-recover-transfers"),
                Some(FileCommand::RecoverClipboardTransfers),
            ));
        }
        spec.push((
            t("files-menu-open-terminal"),
            Some(FileCommand::OpenTerminal),
        ));
        // Remove the current directory from the favourites when it is one.
        if self.current_is_removable_favourite() {
            spec.push((
                t("files-menu-remove-favourite"),
                Some(FileCommand::RemoveFavorite(self.current_path.clone())),
            ));
        }
        // How the folder is shown. Sorting lives here too: a grid has no
        // column header, and "Sort by type" is reached only from this menu.
        spec.push((String::new(), None));
        for (label, by) in [
            ("files-menu-sort-name", SortBy::Name),
            ("files-menu-sort-size", SortBy::Size),
            ("files-menu-sort-type", SortBy::Type),
            ("files-menu-sort-date", SortBy::Modified),
        ] {
            spec.push((t(label), Some(FileCommand::SetSort(by))));
        }
        spec.push((
            if self.show_hidden {
                t("files-menu-hide-hidden")
            } else {
                t("files-menu-show-hidden")
            },
            Some(FileCommand::ToggleHidden),
        ));
        tidy_menu(spec)
    }
}
