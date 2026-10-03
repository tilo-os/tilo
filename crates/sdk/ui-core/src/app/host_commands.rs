//! This app as the host chrome sees it: its commands, and the one local door
//! they enter.

use super::App;
use crate::host_chrome::edit::{EDIT_OPERATION, EDIT_RESOURCE, edit_resource};
#[cfg(any(feature = "native", test))]
use crate::host_chrome::model::{self, HostCommand, RecentChange};
#[cfg(any(feature = "native", test))]
use crate::platform::HostShortcut;
#[cfg(any(feature = "native", test))]
use crate::standalone_history::{HistoryReadContext, StandaloneHistory};

/// Changes whenever a host command's label, enabled state or presence may
/// have, so the host chrome rebuilds only then.
#[cfg(all(feature = "native", not(target_os = "android")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostCommandsKey {
    undo: u64,
    standalone_history: u64,
    framework_history_open: bool,
    palette: bool,
    focus: Option<taffy::NodeId>,
    resources: (u64, u64),
    /// Recent changes show relative times, which age by the minute.
    minute: u64,
}

/// The Edit door: the one undo, reached by keyboard, menu and automation.
impl App {
    /// Run an `tilo.host:edit` operation: the same undo the keyboard does.
    pub(super) fn execute_edit_operation(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        debug_assert_eq!(encoded.type_id, EDIT_OPERATION);
        let request =
            serde_json::from_slice(&encoded.payload).map_err(|_| "Invalid Edit operation")?;
        self.perform_undo_request(request);
        Ok(())
    }

    /// The Edit resource, when `address` names it.
    pub(super) fn edit_resource_for(
        &mut self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        (address.resource == EDIT_RESOURCE).then(|| edit_resource(&self.focused_undo_status()))
    }

    /// Undo or redo for the local author, offered first to a focused
    /// component with its own history (a text field), then to the focused
    /// document. Keyboard, menu and automation all land here.
    pub(crate) fn perform_undo_request(&mut self, request: crate::undo::UndoRequest) {
        if crate::undo::handle_local_undo_request(self.root_widget.as_mut(), request) {
            self.needs_sync = true;
            return;
        }
        // Otherwise round-tripping document undo/redo loses caret position.
        let current_meta = crate::undo::get_state(self.root_widget.as_ref());
        let current_focus_idx = self
            .focused_node
            .and_then(|id| self.focusable_order.iter().position(|x| *x == id));
        let binding = self
            .focused_node
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root_widget.callback_binding());
        let owned_target = binding.is_some()
            || self
                .focused_node
                .is_some_and(|node| self.bindings.owns_document(node));
        let entry = if let Some(binding) = binding {
            binding
                .history_request(&mut self.state.doc, request)
                .ok()
                .flatten()
        } else if let Some(owner) = self
            .focused_node
            .and_then(|node| self.bindings.legacy_for(node))
        {
            crate::bound_dispatch::legacy_history_request(
                &owner,
                request,
                &self.permission_ctx,
                &self.collaboration.local_author,
            )
            .ok()
            .flatten()
        } else {
            let author = Some(&self.collaboration.local_author);
            match request {
                crate::undo::UndoRequest::Undo => {
                    self.state.undo_as(author, current_focus_idx, current_meta)
                }
                crate::undo::UndoRequest::Redo => {
                    self.state.redo_as(author, current_focus_idx, current_meta)
                }
            }
        };
        if let Some(entry) = entry {
            if owned_target {
                self.needs_sync = true;
                self.layout.layout_dirty = true;
            } else {
                self.apply_undo_entry(entry);
            }
        }
    }

    /// What Undo would do for the target the keyboard would undo in. Reads a
    /// snapshot, so a pending edit keeps its undo step open.
    fn focused_undo_status(&mut self) -> crate::undo::UndoStatus {
        let focused = self.focused_node;
        if let Some(binding) = focused
            .and_then(|node| self.bindings.binding_for(node))
            .or_else(|| self.root_widget.callback_binding())
        {
            return binding.undo_status(&mut self.state.doc).unwrap_or_default();
        }
        if let Some(status) = focused.and_then(|node| {
            self.bindings
                .legacy_status(node, &self.collaboration.local_author)
        }) {
            return status;
        }
        self.state
            .undo_status_snapshot(Some(&self.collaboration.local_author))
    }
}

#[cfg(any(feature = "native", test))]
impl App {
    /// The commands the host chrome presents. Recent changes are read from
    /// the document history, so only a History menu asks for them.
    pub(crate) fn host_commands(&mut self, with_recent_changes: bool) -> Vec<HostCommand> {
        let mut resources = vec![edit_resource(&self.focused_undo_status())];
        resources.extend(self.history_local_resources());
        if self.palette_presented()
            && let Some(search) = resources
                .iter_mut()
                .find(|resource| resource.id.as_str() == crate::search::request::SEARCH_RESOURCE)
        {
            // The open palette answers its own shortcut by closing.
            for action in &mut search.actions {
                action.contract.availability = tilo_actions::Availability::Unavailable {
                    code: "search_open".into(),
                    reason: crate::i18n::I18nManager::lookup_plain("host-search"),
                };
            }
        }
        resources.extend(self.collect_declared_resources());
        let recent = if with_recent_changes {
            self.recent_changes()
        } else {
            Vec::new()
        };
        model::host_commands(
            &resources,
            &recent,
            self.input.platform,
            crate::time::wall(),
        )
    }

    #[cfg(all(feature = "native", not(target_os = "android")))]
    pub(crate) fn host_commands_key(&self) -> HostCommandsKey {
        HostCommandsKey {
            undo: self.state.history_revision(),
            standalone_history: self.document_history.revision(),
            framework_history_open: self
                .history
                .as_ref()
                .is_some_and(|history| history.is_open()),
            palette: self.palette_presented(),
            focus: self.focused_node,
            resources: (
                self.actions.resource_generation.get(),
                self.actions.permission_generation.get(),
            ),
            minute: crate::time::wall()
                .duration_since(crate::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs() / 60),
        }
    }

    /// Activate a host command through the same local door the palette and
    /// assistive technology enter. Returns whether the door accepted it.
    pub(crate) fn invoke_host_command(
        &mut self,
        invocation: &tilo_actions::ControlInvocation,
    ) -> bool {
        let argument = (!invocation.arguments.is_empty()).then(|| {
            format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(invocation.arguments.clone())
            )
        });
        self.invoke_local_address(&invocation.address, argument.as_deref())
    }

    /// The command a host shortcut stands for, invoked; `false` when it is
    /// unavailable, so the key can still reach the app.
    pub(crate) fn invoke_host_shortcut(&mut self, service: HostShortcut) -> bool {
        let shortcut = service.shortcut(self.input.platform);
        let invocation = self
            .host_commands(false)
            .into_iter()
            .find(|command| command.enabled && command.shortcut.as_ref() == Some(&shortcut))
            .and_then(|command| command.invocation());
        invocation.is_some_and(|invocation| self.invoke_host_command(&invocation))
    }

    /// The newest changes people made, from whichever History serves this
    /// app. The document's genesis is a version, not a change anyone made.
    fn recent_changes(&self) -> Vec<RecentChange> {
        self.history_rows()
            .iter()
            .rev()
            .filter(|entry| !entry.is_genesis())
            .take(model::RECENT_CHANGES)
            .map(RecentChange::from_entry)
            .collect()
    }

    /// The rows whichever History serves this app lists, in causal order.
    fn history_rows(&self) -> Vec<crate::widget::HistoryEntry> {
        if let Some(history) = &self.history {
            return history.current_entries(&self.state.doc);
        }
        if !self.history_chrome_enabled {
            return Vec::new();
        }
        let Ok(target) = self.capture_history_target() else {
            return Vec::new();
        };
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record: self
                .persistence
                .records
                .as_ref()
                .and_then(|records| records.active()),
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        StandaloneHistory::open(
            target,
            &context,
            crate::history_source::AuthorDirectory::new(),
        )
        .map(|history| history.rows().to_vec())
        .unwrap_or_default()
    }

    fn palette_presented(&self) -> bool {
        self.actions
            .action_palette
            .as_ref()
            .is_some_and(|palette| palette.is_presented())
    }
}
