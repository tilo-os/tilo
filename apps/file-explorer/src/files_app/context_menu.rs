use super::{FilesState, tidy_menu};
use crate::command::FileCommand;
use crate::i18n::t;
use crate::model::SortBy;

impl FilesState {
    #[allow(
        clippy::too_many_lines,
        reason = "one declarative menu specification keeps visibility, enablement, and command identity auditable"
    )]
    pub(super) fn context_menu_spec(&self) -> Vec<(String, Option<FileCommand>)> {
        let has_sel = !self.selection.is_empty();
        let can_paste = self.can_paste_clipboard();
        let in_trash = self.is_trash_view();
        let mut spec: Vec<(String, Option<FileCommand>)> = Vec::new();

        if self.is_recent_view() {
            return self.recent_menu_spec();
        }
        if let Some(spec) = self.applications_menu_spec() {
            return spec;
        }

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
                if self.offers_sharing(entry) {
                    spec.push((String::new(), None));
                    spec.push((t("files-share"), Some(FileCommand::Share)));
                }
                spec.push((
                    t("files-menu-properties"),
                    Some(FileCommand::ShowProperties),
                ));
                if entry.availability.is_some() {
                    spec.push((
                        t("files-availability-title-short"),
                        Some(FileCommand::ShowAvailability),
                    ));
                }
                if entry.document_id.is_some() {
                    spec.push((t("files-version-history"), Some(FileCommand::ShowHistory)));
                }
            }
            return tidy_menu(spec);
        }

        spec.push((t("files-menu-new-folder"), Some(FileCommand::NewFolder)));
        spec.push((t("files-menu-new-file"), Some(FileCommand::NewFile)));
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
        if self.current_is_removable_favourite() {
            spec.push((
                t("files-menu-remove-favourite"),
                Some(FileCommand::RemoveFavorite(self.current_path.clone())),
            ));
        }
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
