//! Portal file-chooser overlay.
//!
//! This component owns the entire modal lifecycle: queued portal requests,
//! the active picker and its reply channel, security-scoped mounting,
//! painting, and exclusive event routing. The shell overlay host only decides
//! where this modal sits in the global z-order.

pub(crate) mod actions;

use crate::shell::state::WorkspaceAuthority as _;
use taffy::TaffyTree;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::shell::chrome_state::Chrome;

/// The portal file chooser's complete on-screen state.
#[derive(Default)]
pub(crate) struct FilePickerOverlay {
    queue: Option<crate::portal::FilePickerQueue>,
    active: Option<Box<ActiveFilePicker>>,
    actions: actions::PickerActions,
}

impl FilePickerOverlay {
    pub(crate) fn actions(&self) -> actions::PickerActions {
        self.actions.clone()
    }

    pub(crate) fn set_queue(&mut self, queue: crate::portal::FilePickerQueue) {
        self.queue = Some(queue);
    }

    pub(crate) fn widget(&self) -> Option<&ui::widget::FilePicker> {
        self.active.as_ref().map(|active| &active.widget)
    }

    pub(crate) fn widget_mut(&mut self) -> Option<&mut ui::widget::FilePicker> {
        self.active.as_mut().map(|active| &mut active.widget)
    }

    /// Resolves a completed picker and mounts the next queued request.
    ///
    /// Only one request is allowed on screen. Requests arriving while a
    /// picker is active are cancelled immediately instead of becoming stale,
    /// unattributed modals later in the session.
    pub(crate) fn pump(&mut self, chrome: Option<&Chrome>) -> bool {
        let mut dirty = self.pump_semantic(chrome);
        let abandoned = self.active.as_ref().is_some_and(|active| {
            active
                .reply
                .as_ref()
                .is_some_and(tokio::sync::oneshot::Sender::is_closed)
        });
        if abandoned {
            self.active = None;
            dirty = true;
        }

        if let Some(active) = self.active.as_mut()
            && let Some(outcome) = active.widget.take_outcome()
        {
            // `self.active` was confirmed `Some` just above; `take()` can
            // only observe that same state, but a real fallback keeps this
            // panic-free regardless.
            if let Some(mut active) = self.active.take() {
                let picked = match outcome {
                    ui::widget::FilePickerOutcome::Selected(paths) => Some(paths),
                    ui::widget::FilePickerOutcome::Cancelled => None,
                };
                active.answer(picked);
                dirty = true;
            }
        }

        let Some(queue) = self.queue.as_ref() else {
            return dirty;
        };
        for request in queue.drain() {
            if self.active.is_some() {
                let _ = request.reply.send(None);
                continue;
            }
            let scope = request
                .scope
                .clone()
                .unwrap_or_else(|| picker_scope_for(chrome, &request.caller));
            let active = ActiveFilePicker::mount(request, scope);
            tracing::info!(
                app = %active.app_id,
                dir = %active.widget.current_dir().display(),
                "portal: file dialog opened"
            );
            self.active = Some(Box::new(active));
            dirty = true;
        }
        self.actions
            .publish(self.active.as_ref().map(|active| actions::Subject {
                id: active.id,
                workspace: active.workspace,
                app: active.app_id.clone(),
            }));
        dirty
    }

    fn pump_semantic(&mut self, chrome: Option<&Chrome>) -> bool {
        let commands = self.actions.drain();
        let dirty = !commands.is_empty();
        for command in commands {
            let result = self.active.as_mut().filter(|active| {
                active.id == command.subject && active.reply.as_ref().is_some_and(|reply| !reply.is_closed())
            }).ok_or_else(|| crate::shell::i18n::t("file-picker-expired")).and_then(|active| {
                let subject = actions::Subject { id: active.id, workspace: active.workspace, app: active.app_id.clone() };
                if !chrome.is_some_and(|chrome| crate::shell::search::providers::file_picker::authorized(&chrome.borrow(), &subject)) {
                    return Err(crate::shell::i18n::t("file-picker-owner-required"));
                }
                active.widget.perform_semantic(command.state.as_deref(), command.operation).and_then(|mut value| {
                    let Some(fields) = value.as_object_mut() else {
                        return Err(ui::widget::file_picker::semantic::PickerRefusal::Unavailable);
                    };
                    fields.insert("request_id".into(), serde_json::json!(active.id));
                    fields.insert("application".into(), serde_json::json!(active.app_id));
                    Ok(value)
                }).map_err(|error| crate::shell::i18n::t(match error {
                    ui::widget::file_picker::semantic::PickerRefusal::Stale => "file-picker-expired",
                    ui::widget::file_picker::semantic::PickerRefusal::InvalidPage => "file-picker-page-unavailable",
                    ui::widget::file_picker::semantic::PickerRefusal::UnavailableEntry => "file-picker-entry-unavailable",
                    ui::widget::file_picker::semantic::PickerRefusal::OutsideScope => "file-picker-outside-scope",
                    ui::widget::file_picker::semantic::PickerRefusal::Unavailable => "file-picker-operation-unavailable",
                }))
            });
            *command.reply.borrow_mut() = Some(result);
        }
        dirty
    }

    pub(crate) fn draw(
        &mut self,
        viewport: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        ui::overlay::scrim::modal(scene, transform, viewport, 1.0, theme);
        active.widget.set_rect(file_picker_rect(viewport));
        active
            .widget
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// Routes an event to the active picker and reports whether the modal
    /// consumed the shell's input. An active picker always consumes input,
    /// even when its inner widget has no action for a particular event.
    /// The picker is dispatched on a tree of its own; `ctx.node_id` and
    /// `ctx.taffy` are not forwarded.
    pub(crate) fn handle_event(
        &mut self,
        viewport: Rect,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.widget.set_rect(file_picker_rect(viewport));
        let mut taffy = TaffyTree::<()>::new();
        // This tree exists only to hand `handle_event` a valid node id; a
        // failure here means we can't dispatch at all, so decline the
        // event instead of unwrapping into a panic.
        let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
            return false;
        };
        active.widget.handle_event(
            event,
            &mut ui::widget::EventCtx {
                taffy: &taffy,
                ..ctx.reborrow(ctx.path, node)
            },
        );
        *ctx.handled = true;
        true
    }
}

/// Centre the file chooser in `viewport`, clamping its fraction so it stays
/// usable on a small output without walling a large one.
pub fn file_picker_rect(viewport: Rect) -> Rect {
    // The dialog's size comes from the toolkit's panel metrics: a sheet wide
    // enough for the sidebar plus a readable list, tall enough for a dozen
    // rows between the header and footer.
    let metrics = ui::tokens::Metrics::BASE;
    let sheet = f64::from(metrics.panel_width_sheet);
    let min_w = sheet;
    let max_w = sheet * 1.6;
    let min_h = f64::from(metrics.panel_width_list);
    let max_h = sheet * 1.15;
    let w = (viewport.width() * 0.62)
        .clamp(min_w, max_w)
        .min(viewport.width());
    let h = (viewport.height() * 0.72)
        .clamp(min_h, max_h)
        .min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - w) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - h) / 2.0;
    Rect::new(x0, y0, x0 + w, y0 + h)
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "both guards already end at the block that binds them, right where `granted` \
              (the value built from them) is handed back to with_granted_paths; nothing to \
              tighten without cloning the tree/capabilities out for no real gain"
)]
fn picker_scope_for(
    chrome: Option<&Chrome>,
    caller: &crate::portal::PortalCaller,
) -> crate::portal::PickerScope {
    let supplied = chrome.and_then(|chrome| chrome.borrow().hosted_inputs.picker_scope.clone());
    let base = supplied.unwrap_or_else(|| {
        #[cfg(all(target_os = "linux", not(test)))]
        {
            crate::portal::PickerScope::for_user()
        }
        #[cfg(any(not(target_os = "linux"), test))]
        {
            crate::portal::PickerScope {
                initial_dir: std::path::PathBuf::new(),
                roots: Vec::new(),
                home: None,
            }
        }
    });
    let Some(chrome) = chrome else {
        return base;
    };
    // Scoped so the `RefCell` borrow guard drops before
    // `with_granted_paths` runs, rather than lingering to the end of the
    // function for data it no longer needs.
    let granted: Vec<std::path::PathBuf> = {
        let state = chrome.borrow();
        let tree = state.tree();
        let Some(workspace) = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == caller.workspace_id)
        else {
            return base;
        };
        workspace
            .app_capabilities(&caller.app_id)
            .into_iter()
            .filter_map(|capability| match &capability.scope {
                collab_types::caps::Scope::FsPath(path) => Some(std::path::PathBuf::from(path)),
                _ => None,
            })
            .collect()
    };
    base.with_granted_paths(granted)
}

/// An on-screen dialog and the portal request it must answer.
struct ActiveFilePicker {
    id: uuid::Uuid,
    workspace: crate::shell::state::WorkspaceId,
    widget: ui::widget::FilePicker,
    app_id: crate::shell::state::AppId,
    multiple: bool,
    reply: Option<tokio::sync::oneshot::Sender<Option<Vec<std::path::PathBuf>>>>,
}

impl ActiveFilePicker {
    fn mount(request: crate::portal::FilePickerRequest, scope: crate::portal::PickerScope) -> Self {
        use ui::widget::file_picker::FileFilter;
        use ui::widget::{FilePicker, FilePickerMode};

        let mut filters = request
            .filters
            .iter()
            .filter(|filter| !filter.is_catch_all())
            .map(|filter| {
                let extensions = filter
                    .extensions
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                FileFilter::new(filter.label.clone(), &extensions)
            })
            .collect::<Vec<_>>();
        filters.push(FileFilter::all());

        let mode = if request.save {
            FilePickerMode::Save
        } else if request.directory {
            FilePickerMode::Directory
        } else {
            FilePickerMode::Open
        };
        let mut widget = FilePicker::new(request.title.clone(), &scope.initial_dir, scope.roots)
            .with_mode(mode)
            .with_multiple(request.multiple)
            .with_accept_label(request.accept_label.clone())
            .with_home(scope.home)
            .with_filters(filters);
        if let Some(name) = request.current_name.as_deref() {
            widget = widget.with_filename(name);
        }
        // The modal takes every key from the moment it is up, so it holds
        // the keyboard focus its painted ring and its semantics report.
        widget.set_focus(true);
        Self {
            widget,
            id: uuid::Uuid::new_v4(),
            workspace: request.caller.workspace_id,
            app_id: request.caller.app_id.clone(),
            multiple: request.multiple,
            reply: Some(request.reply),
        }
    }

    fn answer(&mut self, picked: Option<Vec<std::path::PathBuf>>) {
        let picked = constrain_selection(picked, self.multiple);
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(picked);
        }
    }
}

fn constrain_selection(
    picked: Option<Vec<std::path::PathBuf>>,
    multiple: bool,
) -> Option<Vec<std::path::PathBuf>> {
    picked.map(|mut paths| {
        if !multiple {
            paths.truncate(1);
        }
        paths
    })
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic overlay geometry fixtures use exactly representable values"
)]
mod tests {
    use super::*;
    use crate::portal::{FilePickerRequest, FilePickerSpawner};
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use tokio::sync::oneshot;

    #[test]
    fn mounted_portal_picker_is_declared_in_the_normal_action_catalogue() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("genuine.tapp"), b"public fixture bytes").unwrap();
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
        let workspace = tree.desktop.active_workspace().unwrap().id;
        let chrome = ChromeState::new(tree);
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, _received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "so.tilo.store"),
            title: "Review package".into(),
            save: false,
            directory: false,
            multiple: false,
            accept_label: Some("Review".into()),
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: temp.path().into(),
                roots: vec![temp.path().into()],
                home: None,
            }),
            reply,
        });
        let mut host =
            crate::shell::overlay_host::ShellOverlayHost::new(Box::new(ui::widget::Text::new("")))
                .with_chrome(chrome.clone());
        host.set_portal_queues(
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel),
            queue,
        );
        assert!(host.pump_file_picker());
        assert!(host.file_picker().is_some());
        let doc = automerge::AutoCommit::new();
        let catalog = host.action_catalog(
            "file-picker",
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &crate::shell::search::action_api::AllVisible,
        );
        assert!(
            catalog
                .enumerate()
                .iter()
                .any(|row| row.resource.provider == "file-picker"),
            "the actually mounted portal picker must be operable through the canonical action registry"
        );
        drop(host);
    }

    fn semantic_call(
        host: &mut crate::shell::overlay_host::ShellOverlayHost,
        chrome: &Chrome,
        id: &str,
        args: serde_json::Value,
    ) -> Option<serde_json::Value> {
        use crate::shell::search::{InvocationDisposition, action_api::AllVisible};
        let mut doc = automerge::AutoCommit::new();
        let catalog = host.exact_action_catalog(
            "file-picker",
            id,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        let rows = catalog.enumerate();
        let contract = rows
            .iter()
            .find(|row| row.resource.id == id)
            .unwrap()
            .contract
            .as_ref()
            .unwrap();
        assert!(matches!(
            contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: false }
        ));
        let action = catalog
            .select(&crate::shell::search::action_api::ResourceRef::new(
                "file-picker",
                id,
            ))
            .unwrap();
        let serde_json::Value::Object(args) = args else {
            panic!("fixture arguments must be an object");
        };
        let disposition = action
            .invoke_json(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                chrome,
                &args,
            )
            .ok()?;
        let InvocationDisposition::Pending(ticket) = disposition else {
            panic!("chooser operation must be tracked");
        };
        assert!(ticket().ok()?.is_none(), "accepted is not completed");
        assert!(host.pump_file_picker());
        let receipt = ticket().ok()?.expect("actual chooser operation completed");
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
            panic!("completed receipt");
        };
        assert!(
            contract.result.validate(&output).is_ok(),
            "actual output must match the published schema"
        );
        Some(output.value.unwrap())
    }

    #[test]
    fn semantic_picker_select_confirm_replies_to_the_actual_portal_and_rejects_stale_state() {
        let temp = tempfile::tempdir().unwrap();
        let public = temp.path().join("genuine.tapp");
        std::fs::write(&public, b"public fixture").unwrap();
        let (mut host, chrome, mut received, subject) = semantic_fixture(temp.path(), false, false);
        let inspect = format!("inspect@{subject}");
        let initial = semantic_call(&mut host, &chrome, &inspect, serde_json::json!({})).unwrap();
        assert_eq!(initial["application"], "so.tilo.store");
        assert_eq!(initial["total"], 1);
        let stale = initial["state"].clone();
        assert!(
            semantic_call(
                &mut host,
                &chrome,
                &format!("select@{subject}"),
                serde_json::json!({"state":stale,"entry":"0".repeat(64)})
            )
            .is_none()
        );
        assert!(received.try_recv().is_err());
        let selected = semantic_call(
            &mut host,
            &chrome,
            &format!("select@{subject}"),
            serde_json::json!({"state":stale,"entry":initial["entries"][0]["id"]}),
        )
        .unwrap();
        assert!(selected["entries"][0]["selected"].as_bool().unwrap());
        assert_ne!(selected["state"], stale);
        assert!(
            semantic_call(
                &mut host,
                &chrome,
                &format!("confirm@{subject}"),
                serde_json::json!({"state":stale,"confirm":true})
            )
            .is_none()
        );
        assert!(received.try_recv().is_err());
        let confirmed = semantic_call(
            &mut host,
            &chrome,
            &format!("confirm@{subject}"),
            serde_json::json!({"state":selected["state"],"confirm":true}),
        )
        .unwrap();
        assert_eq!(confirmed["completed"], true);
        assert_eq!(received.try_recv().unwrap(), Some(vec![public]));
        assert!(host.file_picker().is_none());
        assert!(
            host.action_catalog(
                "file-picker",
                &chrome.borrow(),
                &automerge::AutoCommit::new(),
                &automerge::ROOT,
                &crate::shell::search::action_api::AllVisible
            )
            .enumerate()
            .iter()
            .all(|row| row.resource.provider != "file-picker")
        );
        drop(host);
    }

    type MountedPicker = (
        crate::shell::overlay_host::ShellOverlayHost,
        Chrome,
        oneshot::Receiver<Option<Vec<std::path::PathBuf>>>,
        uuid::Uuid,
    );

    fn semantic_fixture(root: &std::path::Path, save: bool, directory: bool) -> MountedPicker {
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
        let workspace = tree.desktop.active_workspace().unwrap().id;
        let chrome = ChromeState::new(tree);
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "so.tilo.store"),
            title: "Review".into(),
            save,
            directory,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: root.into(),
                roots: vec![root.into()],
                home: None,
            }),
            reply,
        });
        let mut host =
            crate::shell::overlay_host::ShellOverlayHost::new(Box::new(ui::widget::Text::new("")))
                .with_chrome(chrome.clone());
        host.set_portal_queues(
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::Channel),
            queue,
        );
        assert!(host.pump_file_picker());
        let catalog = host.action_catalog(
            "file-picker",
            &chrome.borrow(),
            &automerge::AutoCommit::new(),
            &automerge::ROOT,
            &crate::shell::search::action_api::AllVisible,
        );
        let rows = catalog.enumerate();
        let id = &rows
            .iter()
            .find(|row| row.resource.id.starts_with("inspect@"))
            .unwrap()
            .resource
            .id;
        let subject = uuid::Uuid::parse_str(id.split_once('@').unwrap().1).unwrap();
        (host, chrome, received, subject)
    }

    #[test]
    fn semantic_picker_refuses_escape_and_revokes_closed_request() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("permitted");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"must not be enumerated").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let (mut host, chrome, received, subject) = semantic_fixture(&root, false, false);
        let inspected = semantic_call(
            &mut host,
            &chrome,
            &format!("inspect@{subject}"),
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(inspected["entries"].as_array().unwrap().len(), 1);
        assert!(semantic_call(&mut host, &chrome, &format!("navigate@{subject}"), serde_json::json!({"state":inspected["state"],"entry":inspected["entries"][0]["id"]})).is_none());
        assert_eq!(host.file_picker().unwrap().current_dir(), root);
        assert!(
            semantic_call(
                &mut host,
                &chrome,
                &format!("up@{subject}"),
                serde_json::json!({"state":inspected["state"]})
            )
            .is_none()
        );
        let actions = host.file_picker_actions_for_test();
        let pending = actions
            .begin(
                subject,
                None,
                ui::widget::file_picker::semantic::PickerOperation::Inspect { page: 0 },
            )
            .unwrap();
        drop(received);
        assert!(host.pump_file_picker());
        assert!(
            pending().is_err(),
            "closed portal cannot acknowledge an operation"
        );
        assert!(host.file_picker().is_none());
        assert!(actions.subject().is_none());
        assert!(
            actions
                .begin(
                    subject,
                    None,
                    ui::widget::file_picker::semantic::PickerOperation::Cancel
                )
                .is_err()
        );
    }

    #[test]
    fn semantic_picker_queue_is_bounded_and_rechecks_owner_lock() {
        let temp = tempfile::tempdir().unwrap();
        let (mut host, chrome, mut received, subject) = semantic_fixture(temp.path(), false, false);
        let actions = host.file_picker_actions_for_test();
        let mut pending = Vec::new();
        for _ in 0..16 {
            pending.push(
                actions
                    .begin(
                        subject,
                        None,
                        ui::widget::file_picker::semantic::PickerOperation::Inspect { page: 0 },
                    )
                    .unwrap(),
            );
        }
        assert!(
            actions
                .begin(
                    subject,
                    None,
                    ui::widget::file_picker::semantic::PickerOperation::Cancel
                )
                .is_err()
        );
        chrome.borrow_mut().set_session_locked(true);
        assert!(host.pump_file_picker());
        for operation in pending {
            assert!(operation().is_err());
        }
        assert!(received.try_recv().is_err(), "lock cannot grant a file");
        chrome.borrow_mut().set_session_locked(false);
        let cancel = semantic_call(
            &mut host,
            &chrome,
            &format!("cancel@{subject}"),
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(cancel["completed"], true);
        assert_eq!(received.try_recv().unwrap(), None);
    }

    #[test]
    fn semantic_picker_pages_are_bounded() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..35 {
            std::fs::write(temp.path().join(format!("file-{index:02}")), b"public").unwrap();
        }
        let (mut host, chrome, received, subject) = semantic_fixture(temp.path(), false, false);
        let first = semantic_call(
            &mut host,
            &chrome,
            &format!("inspect@{subject}"),
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(first["entries"].as_array().unwrap().len(), 32);
        let second = semantic_call(
            &mut host,
            &chrome,
            &format!("inspect@{subject}"),
            serde_json::json!({"page":1}),
        )
        .unwrap();
        assert_eq!(second["entries"].as_array().unwrap().len(), 3);
        assert!(
            semantic_call(
                &mut host,
                &chrome,
                &format!("inspect@{subject}"),
                serde_json::json!({"page":2})
            )
            .is_none()
        );
        drop(received);
        host.pump_file_picker();
        drop(host);
    }

    #[test]
    fn semantic_picker_save_requires_normal_replacement_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file-00"), b"public").unwrap();
        let (mut host, chrome, mut received, subject) = semantic_fixture(temp.path(), true, false);
        let inspect = semantic_call(
            &mut host,
            &chrome,
            &format!("inspect@{subject}"),
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            semantic_call(
                &mut host,
                &chrome,
                &format!("filename@{subject}"),
                serde_json::json!({"state":inspect["state"],"filename":"../escape"})
            )
            .is_none()
        );
        let named = semantic_call(
            &mut host,
            &chrome,
            &format!("filename@{subject}"),
            serde_json::json!({"state":inspect["state"],"filename":"file-00"}),
        )
        .unwrap();
        let replace = semantic_call(
            &mut host,
            &chrome,
            &format!("confirm@{subject}"),
            serde_json::json!({"state":named["state"],"confirm":true}),
        )
        .unwrap();
        assert_eq!(replace["replace_pending"], true);
        assert!(received.try_recv().is_err());
        assert!(
            semantic_call(
                &mut host,
                &chrome,
                &format!("replace@{subject}"),
                serde_json::json!({"state":replace["state"],"confirm":false})
            )
            .is_none()
        );
        semantic_call(
            &mut host,
            &chrome,
            &format!("replace@{subject}"),
            serde_json::json!({"state":replace["state"],"confirm":true}),
        )
        .unwrap();
        assert_eq!(
            received.try_recv().unwrap(),
            Some(vec![temp.path().join("file-00")])
        );
        drop(host);
    }

    #[test]
    fn semantic_picker_directory_select_returns_the_actual_folder() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("folder");
        std::fs::create_dir(&child).unwrap();
        let (mut host, chrome, mut received, subject) = semantic_fixture(temp.path(), false, true);
        let inspect = semantic_call(
            &mut host,
            &chrome,
            &format!("inspect@{subject}"),
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(inspect["entries"].as_array().unwrap().len(), 1);
        let chosen = semantic_call(
            &mut host,
            &chrome,
            &format!("select@{subject}"),
            serde_json::json!({"state":inspect["state"],"entry":inspect["entries"][0]["id"]}),
        )
        .unwrap();
        semantic_call(
            &mut host,
            &chrome,
            &format!("confirm@{subject}"),
            serde_json::json!({"state":chosen["state"],"confirm":true}),
        )
        .unwrap();
        assert_eq!(received.try_recv().unwrap(), Some(vec![child]));
    }

    #[test]
    fn file_picker_is_clamped_and_replies_with_the_requested_kind_and_count() {
        fn picker_rect_is_centered_and_clamped_to_viewport() {
            let viewport = Rect::new(30.0, 40.0, 330.0, 240.0);
            let picker = file_picker_rect(viewport);
            assert_eq!(picker, viewport);

            let viewport = Rect::new(10.0, 20.0, 2010.0, 1220.0);
            let picker = file_picker_rect(viewport);
            let sheet = f64::from(ui::tokens::Metrics::BASE.panel_width_sheet);
            assert_eq!(
                picker.width(),
                sheet * 1.6,
                "wide enough for sidebar + list, no wider"
            );
            assert_eq!(picker.height(), sheet * 1.15);
            assert_eq!(picker.center(), viewport.center());
        }

        fn single_file_request_cannot_receive_multiple_paths() {
            let paths = vec!["first".into(), "second".into()];
            assert_eq!(constrain_selection(Some(paths), false).unwrap().len(), 1);

            let paths = vec!["first".into(), "second".into()];
            assert_eq!(constrain_selection(Some(paths), true).unwrap().len(), 2);
            assert_eq!(constrain_selection(None, false), None);
        }

        fn directory_request_flows_through_overlay_to_folder_reply() {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().to_path_buf();
            let child = root.join("folder");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(root.join("file.txt"), b"file").unwrap();

            let tree = WorkspaceTree::bootstrap_demo();
            let workspace = tree
                .desktop
                .active_workspace()
                .expect("active workspace")
                .id;
            let chrome = ChromeState::new(tree);
            chrome.borrow_mut().hosted_inputs.picker_scope = Some(crate::portal::PickerScope {
                initial_dir: root.clone(),
                roots: vec![root],
                home: None,
            });

            let queue = crate::portal::FilePickerQueue::new();
            let (reply, mut received) = oneshot::channel();
            queue.spawn(FilePickerRequest {
                caller: crate::portal::PortalCaller::new(workspace, "org.example"),
                title: "Choose project".into(),
                save: false,
                directory: true,
                multiple: false,
                accept_label: Some("Attach folder".into()),
                current_name: None,
                filters: Vec::new(),
                scope: None,
                reply,
            });

            let mut overlay = FilePickerOverlay::default();
            overlay.set_queue(queue);
            assert!(overlay.pump(Some(&chrome)));
            let widget = overlay.widget_mut().expect("mounted picker");
            assert_eq!(widget.confirm_label(), "Attach folder");
            assert_eq!(widget.entries().len(), 1, "files must be hidden");
            assert!(widget.select_by_path(&child));
            widget.click_open();
            assert!(overlay.pump(Some(&chrome)));

            assert_eq!(received.try_recv().unwrap(), Some(vec![child]));
        }

        picker_rect_is_centered_and_clamped_to_viewport();
        single_file_request_cannot_receive_multiple_paths();
        directory_request_flows_through_overlay_to_folder_reply();
    }

    /// Every key the overlay sees, as the compositor hands it over.
    fn press(overlay: &mut FilePickerOverlay, key: &winit::keyboard::Key, modifiers: Modifiers) {
        let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = automerge::AutoCommit::new();
        let mut handled = false;
        let mut focus_target = None;
        let taffy = TaffyTree::<()>::new();
        for pressed in [true, false] {
            let event = crate::shell::test_control::keyboard_input_event(key, pressed);
            assert!(overlay.handle_event(
                viewport,
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: ui::widget::Edits::lend(&mut doc),
                    path: &automerge::ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: taffy::NodeId::from(0_u64),
                    taffy: &taffy,
                },
            ));
        }
    }

    use winit::event::Modifiers;
    use winit::keyboard::{Key, NamedKey};

    #[test]
    fn a_save_over_an_existing_file_waits_for_replace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let photo = root.join("labrador.jpg");
        std::fs::write(&photo, b"original").unwrap();

        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree
            .desktop
            .active_workspace()
            .expect("active workspace")
            .id;
        let chrome = ChromeState::new(tree);
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, mut received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "so.tilo.canvas"),
            title: "Export a picture".into(),
            save: true,
            directory: false,
            multiple: false,
            accept_label: Some("Export".into()),
            current_name: Some("labrador.jpg".into()),
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: root.clone(),
                roots: vec![root.clone()],
                home: None,
            }),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));

        let none = Modifiers::default();
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        overlay.pump(Some(&chrome));
        assert!(received.try_recv().is_err(), "the app hears nothing yet");
        assert_eq!(
            overlay.widget().and_then(|w| w.replace_prompt()),
            Some(photo.as_path())
        );

        // Escape takes back the Save, not the whole chooser.
        press(&mut overlay, &Key::Named(NamedKey::Escape), none);
        overlay.pump(Some(&chrome));
        assert!(received.try_recv().is_err());
        let widget = overlay.widget().expect("chooser still up");
        assert_eq!(widget.replace_prompt(), None);

        // A new name: Ctrl+A selects the suggested one, typing replaces it.
        let mut ctrl = winit::keyboard::ModifiersState::empty();
        ctrl.set(winit::keyboard::ModifiersState::CONTROL, true);
        press(
            &mut overlay,
            &Key::Character("a".into()),
            Modifiers::from(ctrl),
        );
        for ch in ["e", "d", "i", "t", ".", "j", "p", "g"] {
            press(&mut overlay, &Key::Character(ch.into()), none);
        }
        assert_eq!(
            overlay.widget().map(ui::widget::FilePicker::filename),
            Some("edit.jpg")
        );
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        assert!(overlay.pump(Some(&chrome)));
        assert_eq!(
            received.try_recv().unwrap(),
            Some(vec![root.join("edit.jpg")])
        );
        assert_eq!(std::fs::read(&photo).unwrap(), b"original");
    }

    #[test]
    fn replace_answers_with_the_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let photo = root.join("labrador.jpg");
        std::fs::write(&photo, b"original").unwrap();
        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree
            .desktop
            .active_workspace()
            .expect("active workspace")
            .id;
        let chrome = ChromeState::new(tree);
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, mut received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "so.tilo.canvas"),
            title: "Export a picture".into(),
            save: true,
            directory: false,
            multiple: false,
            accept_label: Some("Export".into()),
            current_name: Some("labrador.jpg".into()),
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: root.clone(),
                roots: vec![root],
                home: None,
            }),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        let none = Modifiers::default();
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        press(&mut overlay, &Key::Named(NamedKey::Tab), none);
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        assert!(overlay.pump(Some(&chrome)));
        assert_eq!(received.try_recv().unwrap(), Some(vec![photo]));
    }

    #[test]
    fn a_scope_decided_with_the_request_replaces_the_users_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let granted = temp.path().join("in");
        std::fs::create_dir(&granted).unwrap();
        std::fs::write(granted.join("x.xlsx"), b"sheet").unwrap();
        let elsewhere = temp.path().join("home");
        std::fs::create_dir(&elsewhere).unwrap();

        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree
            .desktop
            .active_workspace()
            .expect("active workspace")
            .id;
        let chrome = ChromeState::new(tree);
        chrome.borrow_mut().hosted_inputs.picker_scope = Some(crate::portal::PickerScope {
            initial_dir: elsewhere.clone(),
            roots: vec![elsewhere],
            home: None,
        });

        let queue = crate::portal::FilePickerQueue::new();
        let (reply, _received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "org.example"),
            title: "Import".into(),
            save: false,
            directory: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: granted.clone(),
                roots: vec![granted.clone()],
                home: None,
            }),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        let widget = overlay.widget_mut().expect("mounted picker");
        assert_eq!(widget.current_dir(), granted.as_path());
        assert!(widget.select_by_path(&granted.join("x.xlsx")));
    }
}
