//! Portal file-chooser overlay.
//!
//! This component owns the dialog's lifecycle: queued portal requests, the
//! active picker and its reply channel, security-scoped mounting, painting,
//! and event routing. The chooser is a system surface, not part of the app:
//! the shell paints it as a system sheet centred on the display the asking
//! window is on, at the sheet's own size whatever that window's size
//! ([`file_picker_rect`]), with the asking window dimmed behind it. It is
//! modal for that window only: the window is closed to input while the
//! sheet waits, every other window and workspace stays usable, and the
//! sheet goes out of sight and comes back with its window. Only a request
//! no window of the app can answer for is modal for the desktop. The shell
//! overlay host says where the asking window is this frame ([`PickerPlace`])
//! and where this sheet sits in the global z-order.

pub(crate) mod actions;

use crate::shell::input_policy::{InputCapture, PointerCapture};
use crate::shell::state::WorkspaceAuthority as _;
use taffy::TaffyTree;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};

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
    /// A press the sheet took is followed to its release, wherever the
    /// pointer has gone by then.
    grabbed: bool,
    /// The session's file index, which the chooser searches every place
    /// the person can reach with; `None` where the session keeps none.
    search: Option<std::rc::Rc<crate::shell::search::providers::FilesProvider>>,
    /// Operations the chooser took while it was still reading (a listing,
    /// the check before confirming): each is answered with what the
    /// chooser shows once its worker is done, never with what it showed
    /// before.
    parked: Vec<(uuid::Uuid, actions::Reply)>,
}

/// Where the chooser sits this frame, as the overlay host sees the window
/// that asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PickerPlace {
    /// A sheet for the asking window. `window` is the whole window group,
    /// dimmed and closed to input; `content` its app area; `display` the
    /// display that window is on, which the sheet centres on; `focused`
    /// while that window holds the keyboard.
    Window {
        window: Rect,
        content: Rect,
        display: Rect,
        focused: bool,
    },
    /// The asking window is out of sight (another workspace, scrolled off
    /// the strip, behind another tab of its group): the sheet waits with it,
    /// painting nothing and taking no input.
    Hidden,
    /// No window of the app asked: a dialog over the display in use, modal
    /// for the desktop because there is no window to hold it.
    Display(Rect),
}

impl PickerPlace {
    pub(crate) fn dialog_rect(self) -> Option<Rect> {
        match self {
            Self::Window { display, .. } => Some(file_picker_rect(display)),
            Self::Hidden => None,
            Self::Display(viewport) => Some(file_picker_rect(viewport)),
        }
    }
}

impl FilePickerOverlay {
    /// The file or folder the open chooser draws at `at`: its name, its
    /// path, and whether it is a folder.
    pub(crate) fn entry_at(&self, at: Point) -> Option<(String, std::path::PathBuf, bool)> {
        let entry = self.active.as_ref()?.widget.entry_at(at)?;
        Some((entry.name.clone(), entry.path.clone(), entry.is_dir))
    }

    pub(crate) fn actions(&self) -> actions::PickerActions {
        self.actions.clone()
    }

    pub(crate) fn set_queue(&mut self, queue: crate::portal::FilePickerQueue) {
        self.queue = Some(queue);
    }

    pub(crate) fn set_search(
        &mut self,
        files: std::rc::Rc<crate::shell::search::providers::FilesProvider>,
    ) {
        self.search = Some(files);
    }

    /// What the open chooser holds back from the windows where `place`
    /// puts it. A sheet holds the keyboard while its window is the focused
    /// one and the pointer over itself and that window (anywhere while a
    /// press it took is held); a chooser no window asked for holds both
    /// everywhere. Everything else goes on as if no chooser were up.
    pub(crate) fn capture(&self, place: PickerPlace) -> InputCapture {
        if self.active.is_none() {
            return InputCapture::NONE;
        }
        match place {
            PickerPlace::Window {
                window, focused, ..
            } => InputCapture {
                keyboard: focused,
                pointer: if self.grabbed {
                    PointerCapture::Everywhere
                } else {
                    PointerCapture::Within(
                        std::iter::once(window).chain(place.dialog_rect()).collect(),
                    )
                },
            },
            PickerPlace::Hidden => InputCapture::NONE,
            PickerPlace::Display(_) => InputCapture::ALL,
        }
    }

    pub(crate) fn widget(&self) -> Option<&ui::widget::FilePicker> {
        self.active.as_ref().map(|active| &active.widget)
    }

    pub(crate) fn widget_mut(&mut self) -> Option<&mut ui::widget::FilePicker> {
        self.active.as_mut().map(|active| &mut active.widget)
    }

    /// The tab of the window the open chooser is a sheet on; `None` when
    /// no chooser is up or no window of the app asked.
    pub(crate) fn asking_tab(&self) -> Option<crate::shell::state::TabId> {
        self.active.as_ref().and_then(|active| active.asking)
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
        // A sheet goes with its window: closing the window that asked
        // answers the request as cancelled.
        if let Some(chrome) = chrome
            && let Some(tab) = self.asking_tab()
            && crate::shell::app_tabs::anchor_for_tab(&chrome.borrow().tree(), tab).is_none()
            && let Some(mut active) = self.active.take()
        {
            active.answer(None);
            dirty = true;
        }
        if self.active.is_none() {
            self.grabbed = false;
        }
        // Recent and a search are read on a worker thread, which wakes the
        // loop when they are in.
        if let Some(active) = self.active.as_mut() {
            dirty |= active.widget.poll_lookups();
        }
        dirty |= self.answer_parked();

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
            let named = named_window(request.parent_window.as_deref());
            let asking = chrome.and_then(|chrome| {
                crate::shell::app_tabs::asking_tab(
                    &chrome.borrow().tree(),
                    request.caller.workspace_id,
                    &request.caller.app_id,
                    named,
                )
            });
            // The chooser runs the search on its worker thread, which opens
            // and reads the index there.
            let search = self.search.as_ref().map(|files| {
                let indexes = files.searcher();
                let search: ui::widget::file_picker::PickerSearch =
                    std::sync::Arc::new(move |query: &str| {
                        indexes.paths_matching(query, ui::widget::file_picker::SEARCH_LIMIT)
                    });
                search
            });
            let active = ActiveFilePicker::mount(
                request,
                Mounting {
                    scope,
                    asking,
                    search,
                },
            );
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
            let result = self
                .active
                .as_mut()
                .filter(|active| {
                    active.id == command.subject
                        && active
                            .reply
                            .as_ref()
                            .is_some_and(|reply| !reply.is_closed())
                })
                .ok_or_else(|| crate::shell::i18n::t("file-picker-expired"))
                .and_then(|active| {
                    let subject = actions::Subject {
                        id: active.id,
                        workspace: active.workspace,
                        app: active.app_id.clone(),
                    };
                    if !chrome.is_some_and(|chrome| {
                        crate::shell::search::providers::file_picker::authorized(
                            &chrome.borrow(),
                            &subject,
                        )
                    }) {
                        return Err(crate::shell::i18n::t("file-picker-owner-required"));
                    }
                    active
                        .widget
                        .perform_semantic(command.state.as_deref(), command.operation)
                        .and_then(|value| active.answer_with(value))
                        .map_err(refusal_text)
                });
            let reading = self
                .active
                .as_ref()
                .is_some_and(|active| active.id == command.subject && active.widget.reading());
            if result.is_ok() && reading {
                self.parked.push((command.subject, command.reply));
            } else {
                *command.reply.borrow_mut() = Some(result);
            }
        }
        dirty
    }

    /// Answer the operations that waited for the chooser's worker, once it
    /// has nothing left to read; a chooser that went away answers that it
    /// has.
    fn answer_parked(&mut self) -> bool {
        if self.parked.is_empty()
            || self
                .active
                .as_ref()
                .is_some_and(|active| active.widget.reading())
        {
            return false;
        }
        for (subject, reply) in std::mem::take(&mut self.parked) {
            let answer = match self.active.as_ref().filter(|active| active.id == subject) {
                Some(active) => active
                    .widget
                    .semantic_inspection(0)
                    .and_then(|value| active.answer_with(value))
                    .map_err(refusal_text),
                None => Err(crate::shell::i18n::t("file-picker-expired")),
            };
            *reply.borrow_mut() = Some(answer);
        }
        true
    }

    pub(crate) fn draw(
        &mut self,
        place: PickerPlace,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(rect) = place.dialog_rect() else {
            return;
        };
        // The dimming says which window waits on the sheet; every other
        // window stays as bright as it is usable.
        let dimmed = match place {
            PickerPlace::Window { window, .. } => window,
            PickerPlace::Display(viewport) => viewport,
            PickerPlace::Hidden => return,
        };
        ui::overlay::scrim::modal(scene, transform, dimmed, 1.0, theme);
        active.widget.set_rect(rect);
        active
            .widget
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// Routes an event to the active picker and reports whether the sheet
    /// took it. A sheet takes the pointer over its window (the window is
    /// closed to input while it waits) and the keyboard while that window
    /// is focused, except the shell's own Super chords, so the person can
    /// leave for another window; everything else goes on as if no chooser
    /// were up. A chooser no window asked for takes every event.
    /// The picker is dispatched on a tree of its own; `ctx.node_id` and
    /// `ctx.taffy` are not forwarded.
    pub(crate) fn handle_event(
        &mut self,
        place: PickerPlace,
        pointer: Option<Point>,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let capture = self.capture(place);
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        let Some(rect) = place.dialog_rect() else {
            return false;
        };
        if let PickerPlace::Window { .. } = place {
            let at = match event {
                WindowEvent::Touch(touch) => Some(ctx.logical(touch.location)),
                _ => pointer,
            };
            let takes = match sheet_input(event) {
                SheetInput::Pointer => at.is_some_and(|at| capture.pointer.at(at)),
                SheetInput::Keys => capture.keyboard && !ctx.modifiers.state().super_key(),
                SheetInput::Other => false,
            };
            if !takes {
                return false;
            }
            if let WindowEvent::MouseInput { state, .. } = event {
                self.grabbed = *state == ElementState::Pressed;
            }
        }
        active.widget.set_rect(rect);
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

/// The window a portal's `parent_window` handle names, while its export
/// lives.
fn named_window(parent_window: Option<&str>) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        parent_window
            .and_then(crate::compositor::window_handles::WindowHandle::parse)
            .and_then(|handle| handle.surface())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = parent_window;
        None
    }
}

/// Which of the asking window's inputs an event is.
enum SheetInput {
    Pointer,
    Keys,
    Other,
}

const fn sheet_input(event: &WindowEvent) -> SheetInput {
    match event {
        WindowEvent::CursorMoved { .. }
        | WindowEvent::MouseInput { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::Touch(_) => SheetInput::Pointer,
        WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => SheetInput::Keys,
        _ => SheetInput::Other,
    }
}

/// The chooser's sheet, centred on the display `viewport`: a fraction of the
/// display clamped to the toolkit's sheet sizes, so it is usable on a small
/// output without walling a large one. It does not depend on the asking
/// window: a chooser asked for by a small window is as roomy as one asked
/// for by a large one. On a display too small for a sheet it fills the
/// display, as every system sheet does there.
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
    /// The tab of the window this is a sheet on.
    asking: Option<crate::shell::state::TabId>,
    multiple: bool,
    reply: Option<tokio::sync::oneshot::Sender<Option<Vec<std::path::PathBuf>>>>,
}

/// What an agent is told when the chooser refuses an operation.
fn refusal_text(error: ui::widget::file_picker::semantic::PickerRefusal) -> String {
    use ui::widget::file_picker::semantic::PickerRefusal;
    crate::shell::i18n::t(match error {
        PickerRefusal::Stale => "file-picker-expired",
        PickerRefusal::InvalidPage => "file-picker-page-unavailable",
        PickerRefusal::UnavailableEntry => "file-picker-entry-unavailable",
        PickerRefusal::OutsideScope => "file-picker-outside-scope",
        PickerRefusal::Unavailable => "file-picker-operation-unavailable",
    })
}

impl ActiveFilePicker {
    /// An operation's answer, naming the request and the app it is for.
    fn answer_with(
        &self,
        mut value: serde_json::Value,
    ) -> Result<serde_json::Value, ui::widget::file_picker::semantic::PickerRefusal> {
        let Some(fields) = value.as_object_mut() else {
            return Err(ui::widget::file_picker::semantic::PickerRefusal::Unavailable);
        };
        fields.insert("request_id".into(), serde_json::json!(self.id));
        fields.insert("application".into(), serde_json::json!(self.app_id));
        Ok(value)
    }

    fn mount(request: crate::portal::FilePickerRequest, mounting: Mounting) -> Self {
        use ui::widget::file_picker::FileFilter;
        use ui::widget::{FilePicker, FilePickerMode};
        let Mounting {
            scope,
            asking,
            search,
        } = mounting;

        // The widget offers a type menu only when the app named more than
        // one type, with All files after them.
        let filters = request
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

        let mode = if request.save {
            FilePickerMode::Save
        } else if request.directory {
            FilePickerMode::Directory
        } else {
            FilePickerMode::Open
        };
        // The chooser lists, checks and searches on a worker, which wakes
        // the compositor loop when it answers.
        let mut widget = FilePicker::new(request.display_title(), &scope.initial_dir, scope.roots)
            .with_waker(Some(crate::shell::search::invocation_wake::waker()))
            .with_mode(mode)
            .with_multiple(request.multiple)
            .with_accept_label(Some(request.confirm_label()))
            .with_home(scope.home)
            .with_filters(filters)
            .with_requester(requester(&request.caller.app_id))
            .with_search(search)
            .with_recent(request.recent);
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
            asking,
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

/// What the shell decided about a request before mounting it: where the
/// dialog may go, the window it is a sheet on, and the search it offers.
struct Mounting {
    scope: crate::portal::PickerScope,
    asking: Option<crate::shell::state::TabId>,
    search: Option<ui::widget::file_picker::PickerSearch>,
}

/// The app asking, as the sheet names it: its name and its icon. `None`
/// when the shell does not know its name; the sheet then says only what is
/// asked.
fn requester(app_id: &str) -> Option<ui::widget::file_picker::Requester> {
    let name = crate::shell::launcher::display_name_for_app_id_without_waiting(app_id)
        .filter(|name| !name.trim().is_empty())?;
    Some(ui::widget::file_picker::Requester {
        name,
        glyph: ui::icons::app_glyph(app_id, None),
        image: crate::app_icons::for_window_app_id(app_id),
    })
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("genuine.tapp"), b"public fixture bytes").unwrap();
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.own_workspaces_for_test(crate::shell::state::local_user_id_for_room);
        let workspace = tree.desktop.active_workspace().unwrap().id;
        let chrome = ChromeState::new(tree);
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, _received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "so.tilo.store"),
            parent_window: None,
            title: "Review package".into(),
            save: false,
            directory: false,
            writable_directory: false,
            multiple: false,
            accept_label: Some("Review".into()),
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: temp.path().into(),
                roots: vec![temp.path().into()],
                home: None,
            }),
            recent: Vec::new(),
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
        // An operation that leaves the chooser reading (a listing, the
        // check before confirming) completes once its worker has answered.
        let receipt =
            tilo_test_env::wait_for("the chooser operation to complete", || match ticket() {
                Ok(Some(receipt)) => Some(Some(receipt)),
                Ok(None) => {
                    host.pump_file_picker();
                    None
                }
                Err(_) => Some(None),
            })?;
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
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
        tree.own_workspaces_for_test(crate::shell::state::local_user_id_for_room);
        let workspace = tree.desktop.active_workspace().unwrap().id;
        let chrome = ChromeState::new(tree);
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "so.tilo.store"),
            parent_window: None,
            title: "Review".into(),
            save,
            directory,
            writable_directory: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: root.into(),
                roots: vec![root.into()],
                home: None,
            }),
            recent: Vec::new(),
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
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
        // Who owns the workspace is read from the identity the environment
        // names; hold it still while the picker is driven.
        let _env = tilo_test_env::ScopedEnv::new();
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
    fn a_picker_requested_by_a_small_window_opens_at_the_sheet_size_centred_on_its_output() {
        let output = Rect::new(1920.0, 0.0, 1920.0 + 2560.0, 1440.0);
        let sheet = f64::from(ui::tokens::Metrics::BASE.panel_width_sheet);
        let place = |window: Rect| PickerPlace::Window {
            window,
            content: window,
            display: output,
            focused: true,
        };

        let small = Rect::new(2000.0, 100.0, 2400.0, 400.0);
        let rect = place(small).dialog_rect().expect("the sheet is on screen");
        assert!(
            rect.width() >= sheet
                && rect.height() >= f64::from(ui::tokens::Metrics::BASE.panel_width_list),
            "a 400x300 window still gets the sheet size, not its own: {rect:?}"
        );
        assert!(
            rect.width() > small.width() && rect.height() > small.height(),
            "the sheet is not squeezed into the window: {rect:?}"
        );
        assert_eq!(rect.center(), output.center(), "centred on its output");
        assert_eq!(
            rect,
            file_picker_rect(output),
            "the system sheet's own size"
        );

        let large = Rect::new(1940.0, 20.0, 4400.0, 1400.0);
        assert_eq!(
            place(large).dialog_rect(),
            Some(rect),
            "the same sheet whatever the window's size"
        );
        assert_eq!(PickerPlace::Hidden.dialog_rect(), None);
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
                parent_window: None,
                title: "Choose project".into(),
                save: false,
                directory: true,
                writable_directory: false,
                multiple: false,
                accept_label: Some("Attach folder".into()),
                current_name: None,
                filters: Vec::new(),
                scope: None,
                recent: Vec::new(),
                reply,
            });

            let mut overlay = FilePickerOverlay::default();
            overlay.set_queue(queue);
            assert!(overlay.pump(Some(&chrome)));
            settle(&mut overlay, &chrome);
            let widget = overlay.widget_mut().expect("mounted picker");
            assert_eq!(widget.confirm_label(), "Attach folder");
            assert_eq!(widget.entries().len(), 1, "files must be hidden");
            assert!(widget.select_by_path(&child));
            widget.click_open();
            settle(&mut overlay, &chrome);

            assert_eq!(received.try_recv().unwrap(), Some(vec![child]));
        }

        picker_rect_is_centered_and_clamped_to_viewport();
        single_file_request_cannot_receive_multiple_paths();
        directory_request_flows_through_overlay_to_folder_reply();
    }

    #[test]
    fn writable_directory_picker_renders_system_owned_consent() {
        let temp = tempfile::tempdir().unwrap();
        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active_workspace().unwrap().id;
        let chrome = ChromeState::new(tree);
        chrome.borrow_mut().hosted_inputs.picker_scope = Some(crate::portal::PickerScope {
            initial_dir: temp.path().to_path_buf(),
            roots: vec![temp.path().to_path_buf()],
            home: None,
        });
        let queue = crate::portal::FilePickerQueue::new();
        let (reply, mut received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "org.example"),
            parent_window: None,
            title: "Open folder".into(),
            save: false,
            directory: true,
            writable_directory: true,
            multiple: false,
            accept_label: Some("Open".into()),
            current_name: None,
            filters: Vec::new(),
            scope: None,
            recent: Vec::new(),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        let picker = overlay.widget_mut().unwrap();
        assert_eq!(
            picker.confirm_label(),
            crate::shell::i18n::t("portal-file-allow-write-folder")
        );
        let node = ui::Widget::accessibility(picker).unwrap();
        assert_eq!(
            node.name,
            Some(crate::shell::i18n::t("portal-file-grant-write-folder"))
        );
        assert!(
            received.try_recv().is_err(),
            "showing the request is not consent"
        );
        picker.click_cancel();
        assert!(overlay.pump(Some(&chrome)));
        assert_eq!(received.try_recv().unwrap(), None);
    }

    /// Pump the overlay until the chooser's worker has answered everything
    /// it was asked, as the compositor does each time the worker wakes it.
    fn settle(overlay: &mut FilePickerOverlay, chrome: &Chrome) {
        tilo_test_env::wait_until("the chooser's worker to answer", || {
            overlay.pump(Some(chrome));
            !overlay
                .widget()
                .is_some_and(ui::widget::FilePicker::reading)
        });
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
                PickerPlace::Display(viewport),
                None,
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
            parent_window: None,
            title: "Export a picture".into(),
            save: true,
            directory: false,
            writable_directory: false,
            multiple: false,
            accept_label: Some("Export".into()),
            current_name: Some("labrador.jpg".into()),
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: root.clone(),
                roots: vec![root.clone()],
                home: None,
            }),
            recent: Vec::new(),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        settle(&mut overlay, &chrome);

        let none = Modifiers::default();
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        settle(&mut overlay, &chrome);
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
        settle(&mut overlay, &chrome);
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
            parent_window: None,
            title: "Export a picture".into(),
            save: true,
            directory: false,
            writable_directory: false,
            multiple: false,
            accept_label: Some("Export".into()),
            current_name: Some("labrador.jpg".into()),
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: root.clone(),
                roots: vec![root],
                home: None,
            }),
            recent: Vec::new(),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        settle(&mut overlay, &chrome);
        let none = Modifiers::default();
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        settle(&mut overlay, &chrome);
        press(&mut overlay, &Key::Named(NamedKey::Tab), none);
        press(&mut overlay, &Key::Named(NamedKey::Enter), none);
        settle(&mut overlay, &chrome);
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
            parent_window: None,
            title: "Import".into(),
            save: false,
            directory: false,
            writable_directory: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: granted.clone(),
                roots: vec![granted.clone()],
                home: None,
            }),
            recent: Vec::new(),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        settle(&mut overlay, &chrome);
        let widget = overlay.widget_mut().expect("mounted picker");
        assert_eq!(widget.current_dir(), granted.as_path());
        assert!(widget.select_by_path(&granted.join("x.xlsx")));
    }
}
