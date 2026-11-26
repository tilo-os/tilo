//! The `AppWidget` trait — the single contract between the tilo shell and any in-process app hosted
//! inside a tile. Extends the base `Widget` trait with app-level metadata (kind, title, icon,
//! permission hint, commands).

use std::cell::RefCell;
use std::rc::Weak;
use tilo_ui_base::widget::Widget;

/// The person's answer to [`AppProcessIntent::AllowHostShell`]: `None` until they choose, then whether
/// the device's own shell was allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostShellAllow {
    pub answer: Option<bool>,
}

/// A typed, device-local request from app content to shell chrome.
#[derive(Debug, Clone)]
pub enum AppProcessIntent {
    ShowWorkspaceActivity {
        resource_id: String,
        version_id: Option<[u8; 16]>,
        /// A factual app-native event that may not yet be part of the shell's CRDT record index.
        #[cfg(feature = "model")]
        entry: Option<crate::widget::HistoryEntry>,
    },
    /// Show what an agent's task run did: History, at the run (`group`, its
    /// `runtime_collab::task_run_group`), listed as one unit with Undo for each step and for the run.
    ShowTaskRun { group: String },
    /// Open the platform search scoped to this app (or one part of it), with the query already typed.
    OpenSearch(crate::widget::search_request::SearchRequest),
    /// Terminal asks for the device's own shell, which the person has not allowed on this device yet.
    AllowHostShell {
        reply: Weak<RefCell<HostShellAllow>>,
    },
}

impl AppProcessIntent {
    /// The app-native event a [`Self::ShowWorkspaceActivity`] carries.
    #[cfg(feature = "model")]
    fn activity_entry(&self) -> Option<&crate::widget::HistoryEntry> {
        match self {
            Self::ShowWorkspaceActivity { entry, .. } => entry.as_ref(),
            _ => None,
        }
    }

    /// Without Tilo's model an activity carries no app-native event.
    #[cfg(not(feature = "model"))]
    #[allow(clippy::unused_self, reason = "the model build reads the event")]
    const fn activity_entry(&self) -> Option<()> {
        None
    }
}

impl PartialEq for AppProcessIntent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                left @ Self::ShowWorkspaceActivity {
                    resource_id: left_resource,
                    version_id: left_version,
                    ..
                },
                right @ Self::ShowWorkspaceActivity {
                    resource_id: right_resource,
                    version_id: right_version,
                    ..
                },
            ) => {
                left_resource == right_resource
                    && left_version == right_version
                    && left.activity_entry() == right.activity_entry()
            }
            (Self::OpenSearch(left), Self::OpenSearch(right)) => left == right,
            (
                Self::AllowHostShell { reply: left_reply },
                Self::AllowHostShell { reply: right_reply },
            ) => Weak::ptr_eq(left_reply, right_reply),
            _ => false,
        }
    }
}

/// A lightweight icon descriptor usable before the full asset pipeline lands.
#[derive(Debug, Clone)]
pub enum IconDescriptor {
    Glyph(&'static str),
    Named(String),
}

/// An [`AppCommand::invoke`] handler.
pub type AppCommandInvoke =
    Box<dyn Fn(&mut automerge::AutoCommit, &automerge::ObjId) + Send + Sync>;

/// A command palette entry contributed by an app.
pub struct AppCommand {
    pub id: String,
    pub label: String,
    pub shortcut: Option<String>,
    pub invoke: AppCommandInvoke,
}

impl std::fmt::Debug for AppCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCommand")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("shortcut", &self.shortcut)
            .field("invoke", &"<fn>")
            .finish()
    }
}

/// What kind of app a window holds, for the mark it wears.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppKind {
    Terminal,
    Files,
    Editor,
    Browser,
    Settings,
    /// Any other app, by its own glyph from the icon set
    /// ([`tilo_ui_base::icons::app_glyph`]): a window wears its own app's mark,
    /// never another app's.
    App(tilo_ui_base::icons::Icon),
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::Files => "Files",
            AppKind::Editor => "Editor",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
            AppKind::App(_) => "App",
        }
    }
}

impl tilo_ui_base::widget::identity::AsDynWidget for dyn AppWidget {
    fn as_dyn_widget(&self) -> &dyn Widget {
        self
    }

    fn as_dyn_widget_mut(&mut self) -> &mut dyn Widget {
        self
    }
}

tilo_ui_base::draw_child_of_dyn!(AppWidget);

/// Trait every in-process app implements so the shell can host it inside a `TileContent::App`.
pub trait AppWidget: Widget {
    /// Capture app-owned local presentation alongside the ordinary View.
    #[cfg(feature = "model")]
    fn capture_application_view(
        &self,
    ) -> Result<Option<crate::application_view::ApplicationLocalView>, crate::view::ThawError> {
        Ok(None)
    }
    /// Prepare current topology from an immutable latest-record loan.
    #[cfg(feature = "model")]
    fn restore_application_view(
        &mut self,
        _local: &crate::application_view::ApplicationLocalView,
        _doc: &automerge::AutoCommit,
    ) -> Result<crate::application_view::ApplicationFocusRestore, crate::view::ThawError> {
        Err(crate::view::ThawError::State(
            "application-local restoration unsupported".into(),
        ))
    }
    /// Finish after ordinary View restoration and before any mount or capture.
    fn finish_application_view_restore(&mut self) -> Result<(), crate::view::ThawError> {
        Ok(())
    }

    fn app_kind(&self) -> AppKind;
    fn app_title(&self) -> &str;
    fn app_icon(&self) -> IconDescriptor;
    /// Which role permission applies to this app.
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
    /// Command-palette contributions.
    fn commands(&self) -> Vec<AppCommand> {
        vec![]
    }

    /// Framework-owned durability, delivered only after the record loan ends. Returns whether the
    /// window shows anything new of it (its bar's notice or unsaved mark): a save that lands within
    /// [`crate::save_status::SAVE_STALL`] shows nothing.
    #[cfg(feature = "model")]
    fn resource_save_status(&mut self, _status: crate::resource::ResourceSaveStatus) -> bool {
        false
    }

    /// The workspace refused a change this record made, and the record withdrew it: the action is refused as
    /// one refused on this device is.
    #[cfg(feature = "model")]
    fn record_refused(&mut self, _refusal: &crate::resource::ResourceRefusal) {}

    /// What the app said it did since last asked ("Added Buy milk"), for the record to hold until the
    /// workspace keeps it ([`crate::resource::ResourceStore::hold_notice`]).
    fn take_notices(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// The workspace kept what `text` says the app did: say it now.
    fn record_said(&mut self, _text: &str) {}

    /// Answer a device-owner operation from the authenticated hosting shell.
    fn device_shell_operation(
        &mut self,
        _operation: &crate::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        Err("This application has no device shell".into())
    }

    /// Handle the trusted shell owner selecting the device shell, without granting it.
    fn request_device_shell(&mut self) -> bool {
        false
    }

    /// Whether the person asked to try a failed record save again ([`crate::SaveAction::Retry`] on the
    /// [`crate::SaveStatusSurface`]), once.
    fn take_save_retry(&mut self) -> bool {
        false
    }

    /// Drain a one-shot request for shell-owned chrome.
    fn take_process_intent(&mut self) -> Option<AppProcessIntent> {
        None
    }

    /// The component to show in this one's place, built now: what a root shown before its record
    /// ([`crate::application::ProcessContext::resource_opening`]) answers once the record is had.
    #[cfg(feature = "model")]
    fn take_component_replacement(&mut self) -> Option<crate::application::ComponentReplacement> {
        None
    }

    /// The narrowest window this app is still usable in, in logical px: a Tilo app's compact layout, a
    /// client's `xdg_toplevel` min-size hint.
    fn min_window_width(&self) -> Option<f64> {
        tilo_ui_base::widget::minimum_window_width(self)
    }

    /// The widest window this app can fill, in logical px: a client's `xdg_toplevel` max-size hint.
    fn max_window_width(&self) -> Option<f64> {
        None
    }

    /// A person is dragging one edge of this app's window; `fixed` is the edge that holds still on
    /// screen (`None` once nothing holds).
    fn set_live_resize_anchor(&mut self, _fixed: Option<crate::widget::WindowEdge>) {}

    /// The edge drag ended.
    fn release_live_resize_anchor(&mut self) {}

    /// What a private window offers its own device: the resources and actions its API publishes, for
    /// the host to list and invoke for this device's person without publishing them to any workspace.
    fn device_local_resources(&self) -> Option<Vec<tilo_actions::ResourceSpec>> {
        None
    }

    /// Invoke one of [`Self::device_local_resources`]' actions as the window's own person, through its
    /// API.
    fn invoke_device_local_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        Err(tilo_actions::ApiError::Unknown(address.clone()))
    }

    /// Invoke one of [`Self::device_local_resources`]' actions as `actor` (an agent or a script),
    /// or as the window's own person when `None`: the same API, with the invocation admitted for
    /// whoever asked, so an action that acts on this device's person alone is refused for anyone
    /// else. A window that keeps no such distinction answers as it does for the person.
    fn invoke_this_device_action_as(
        &mut self,
        _actor: Option<&tilo_actions::Actor>,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError> {
        self.invoke_device_local_action(address, arguments)
    }

    /// What an app keeping its own document publishes for the search of its window: its resources and
    /// its search providers, which its host cannot read through its widgets.
    fn window_search(&self) -> Option<WindowPublication> {
        None
    }

    /// Run `invocation`, chosen in the search of this window, as the app's own control would, for an
    /// app keeping its own document.
    fn run_window_control(
        &mut self,
        invocation: tilo_actions::ControlInvocation,
    ) -> Result<(), tilo_actions::ControlInvocation> {
        Err(invocation)
    }
}

/// What a window publishes for its own search: the resources it holds and can act on, and the
/// search providers that find what it keeps.
pub struct WindowPublication {
    pub resources: Vec<tilo_actions::ResourceSpec>,
    pub providers: Vec<Box<dyn crate::search::SearchProvider>>,
}

/// The draw environment a mounted component's root draws in: the outer one,
/// lent the component's own authority over its widgets.
#[cfg(feature = "model")]
fn lent_permissions<'a>(
    env: &tilo_ui_base::draw::DrawEnv<'a>,
    binding: &crate::bound_dispatch::CallbackBinding,
) -> tilo_ui_base::draw::DrawEnv<'a> {
    let permissions = binding.permissions.borrow().clone();
    let mut inner = env.clone();
    inner.permissions = Some(std::rc::Rc::new(permissions));
    inner.paint_permissions = tilo_ui_base::permissions::shared_revision();
    inner
}

#[cfg(feature = "model")]
mod component_adapter {
    /// Everything a component's last pass read, as it stood once the pass ended.
    pub(crate) struct Observation {
        pub(crate) revision: u64,
        /// Its record's committed heads and what is pending on top of them.
        pub(crate) document: (Vec<automerge::ChangeHash>, usize),
        pub(crate) inputs: PassInputs,
        pub(crate) reads: tilo_ui_base::component_inputs::Reads,
    }

    /// What a component's pass reads of its host besides its record: what its permissions mean now and its
    /// record's save status (what its root was last told, in an observation; what the store says, now), as a
    /// frame's quiet check reads them, and the locale.
    #[derive(PartialEq)]
    pub(crate) struct PassInputs {
        pub(crate) owner: crate::bound_dispatch::OwnerInputs,
        pub(crate) locale: u64,
    }

    impl Observation {
        /// Why a pass reading `now` would read something else than the pass this observation was taken after,
        /// or `None` when it would read the same.
        pub(crate) fn moved(&self, now: &Now) -> Option<&'static str> {
            if self.document.1 != 0 {
                return Some("ui.component.loud.uncommitted");
            }
            if now.revision != Some(self.revision) {
                return Some("ui.component.loud.input");
            }
            if now.document.as_ref() != Some(&self.document) {
                return Some("ui.component.loud.document");
            }
            if now.inputs != self.inputs {
                return Some("ui.component.loud.inputs");
            }
            self.reads.stand().err()
        }
    }

    /// What a component's observation is compared with: the same readings, now.
    pub(crate) struct Now {
        pub(crate) revision: Option<u64>,
        pub(crate) document: Option<(Vec<automerge::ChangeHash>, usize)>,
        pub(crate) inputs: PassInputs,
    }

    use super::{AppProcessIntent, AppWidget, IconDescriptor};
    use crate::application::AppComponent;
    use crate::floating_surface::{FloatingSurfaceProvider, FloatingSurfaceWidgetExt};
    use crate::{time::Instant, widget::AppKind};
    use tilo_ui_base::widget::DrawChild;
    use tilo_ui_base::widget::Widget;
    use tilo_ui_base::{
        widget::ActionDirectoryResourcePage, widget::Affine, widget::AudiencePresentationProvider,
        widget::AudiencePresentationWidgetExt, widget::AutoCommit, widget::CompleteContentProvider,
        widget::CompleteContentWidgetExt, widget::ControlBindingMetadata,
        widget::FullscreenProvider, widget::FullscreenWidgetExt, widget::HashMap, widget::HashSet,
        widget::NodeId, widget::ObjId, widget::PresenceKey, widget::ResourceOperationStart,
        widget::Scene, widget::TaffyTree, widget::TaffyTreeExt, widget::TextRenderer,
        widget::Theme, widget::WindowEvent,
    };

    pub struct BoundAppWidget {
        pub(crate) component: AppComponent,
        /// A complete-content capture laid the root out in a tree of its own: the host rebuilds its layout
        /// before the next frame.
        relayout: bool,
        /// The record's durability changed and the root synced with it
        /// ([`AppComponent::take_save_status_changed`]): what shows it is drawn by the next frame.
        save_status_shown: bool,
        /// What the root's last pass read, as it stood once the pass ended: a pass that would read the same
        /// is skipped ([`tilo_ui_base::component_inputs`]).
        observation: Option<Observation>,
        /// The root asked for its next sync whatever its observation says: it was laid out anew, its update or
        /// a deadline changed it.
        owes_sync: bool,
        /// A sync ran since the last settle: what it took up is written by the next settle.
        owes_settle: bool,
        /// The size the root was last laid out at: a new size is input its next sync takes up.
        laid_out: Option<vello::kurbo::Size>,
        /// How a person reaches the window, as the window was last told: telling it again is
        /// no input.
        environment: Option<tilo_ui_base::interaction::InteractionEnvironment>,
        /// What the root's updates read since its last sync: a view built from what moved is input the sync
        /// it precedes takes up, and a build that loads the words it shows proves nothing
        /// ([`tilo_ui_base::component_inputs`]).
        update_reads: Option<tilo_ui_base::component_inputs::Reads>,
    }

    impl BoundAppWidget {
        pub fn new(component: AppComponent) -> Self {
            Self {
                component,
                relayout: false,
                save_status_shown: false,
                observation: None,
                owes_sync: true,
                owes_settle: true,
                laid_out: None,
                environment: None,
                update_reads: None,
            }
        }

        /// What the root's next pass would read, now.
        #[inline(never)]
        fn inputs_now(&self, outer: &AutoCommit) -> Now {
            let binding = self.component.callback_binding();
            Now {
                revision: binding.lifetime.revision(),
                document: binding
                    .read_document(outer, |document, _| {
                        tilo_ui_base::doc::settle_mark(document)
                    })
                    .ok(),
                inputs: Self::inputs(&self.component, &binding),
            }
        }

        fn inputs(
            component: &AppComponent,
            binding: &crate::bound_dispatch::CallbackBinding,
        ) -> PassInputs {
            PassInputs {
                owner: crate::bound_dispatch::OwnerInputs {
                    permissions: binding.permissions.borrow().directory_observation(),
                    save: component.record_save_now().ok_or_else(String::new),
                },
                locale: tilo_ui_base::i18n::I18nManager::revision(),
            }
        }

        /// Whether the root's next pass would read only what its last one read: nothing reached the
        /// component, nothing it follows moved, and it asked for nothing.
        fn at_rest(&self, outer: &AutoCommit) -> bool {
            if self.owes_sync || tilo_ui_base::component_inputs::forced() {
                return false;
            }
            let Some(observation) = &self.observation else {
                return false;
            };
            let binding = self.component.callback_binding();
            if binding.records.is_opening(&binding.checkout.id) {
                return false;
            }
            let Some(root) = self.component.root.as_deref() else {
                return false;
            };
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let now = crate::time::now();
            if Self::busy(root) || root.next_wake_deadline().is_some_and(|due| due <= now) {
                return false;
            }
            drop(_entry);
            let moved = observation.moved(&self.inputs_now(outer));
            if let Some(why) = moved {
                drop(tilo_actions::profiling::Phase::start(why));
            }
            moved.is_none()
        }

        /// The root's sync, from the component's record.
        fn sync_root(&mut self, state: &AutoCommit) {
            let binding = self.component.callback_binding();
            if binding.records.is_opening(&binding.checkout.id) {
                if let Some(root) = self.component.root.as_deref_mut() {
                    root.sync(state, &automerge::ROOT);
                }
                return;
            }
            let sync_root = |component: &mut AppComponent| {
                binding
                    .read_document(state, |document, resource_root| {
                        if let Some(root) = component.root.as_deref_mut() {
                            root.sync(document, resource_root);
                        }
                    })
                    .unwrap_or(());
            };
            // The record's durability and what the workspace refused are
            // delivered before the root syncs: the one sync takes up what the
            // owner now shows. Delivered after it, a save that landed between
            // two passes would run the whole tree through a second sync in the
            // same frame for nothing.
            self.component.project_resource_save_status();
            sync_root(&mut self.component);
            // A status the sync itself moved (an app's own save, the retry it
            // made) is shown by syncing the root once more with it: the frame
            // needs no rebuilt tree, which repaints the whole window.
            if self.component.project_resource_save_status() {
                sync_root(&mut self.component);
            }
        }

        /// The root's settle: what its widgets took up is written to the record, and shown in the same
        /// frame.
        fn settle_root(&mut self, edits: &mut tilo_ui_base::widget::Edits<'_>) {
            let binding = self.component.callback_binding();
            binding
                .with_settlement(edits.document_mut(), |document, resource_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    let before = tilo_ui_base::doc::settle_mark(document);
                    root.settle(
                        &mut tilo_ui_base::widget::Edits::lend(document),
                        resource_root,
                    );
                    let _ = binding.drain_owned_tree(root, document);
                    if tilo_ui_base::doc::settle_mark(document) != before {
                        root.update(document, resource_root);
                        root.sync(document, resource_root);
                    }
                })
                .unwrap_or(());
        }

        /// A debug build's proof that a component at rest skipped nothing: its sync runs anyway, and must
        /// leave what it shows, its record and its layout as they were.
        #[cfg(debug_assertions)]
        fn verify_rest(&mut self, state: &AutoCommit) {
            let shown = |widget: &Self| {
                (
                    widget
                        .in_root(|root| crate::headless::widget_inspection_json(root))
                        .unwrap_or_default(),
                    widget
                        .component
                        .callback_binding()
                        .read_document(state, |document, _| {
                            tilo_ui_base::doc::settle_mark(document)
                        })
                        .ok(),
                    widget
                        .in_root(|root| root.layout_needs_measure() || root.is_layout_dirty())
                        .unwrap_or_default(),
                    {
                        let binding = widget.component.callback_binding();
                        (
                            binding.queued_controls.borrow().len(),
                            binding.search_requests.borrow().len(),
                            widget.component.replacement_waits(),
                        )
                    },
                )
            };
            // Work off the frame that lands while the check's own pass runs
            // (a save the store finished) wakes the component for its next
            // frame: what the pass shows of it was not owed yet. So does a
            // language that moves while it runs: the observation the pass ends
            // with is the language it read, so the next pass reads it again.
            let crossed = self.component.callback_binding().lifetime.crossed();
            let locale = tilo_ui_base::i18n::I18nManager::revision();
            let ((before, after), reads) = tilo_actions::profiling::unobserved(|| {
                let binding = self.component.callback_binding();
                let before = shown(self);
                let ((), reads) = crate::resource::reads::reading(&binding.lifetime, || {
                    tilo_ui_base::component_inputs::proving(|| self.sync_root(state));
                });
                ((before, shown(self)), reads)
            });
            self.observation_pass(state, reads);
            let consistent = before == after
                || self.component.callback_binding().lifetime.crossed() != crossed
                || tilo_ui_base::i18n::I18nManager::revision() != locale;
            if consistent {
                return;
            }
            let differs = before
                .0
                .bytes()
                .zip(after.0.bytes())
                .position(|(was, now)| was != now)
                .unwrap_or_else(|| before.0.len().min(after.0.len()));
            let excerpt = |text: &str| {
                let start = text.floor_char_boundary(differs.saturating_sub(200));
                let end = text.floor_char_boundary(differs.saturating_add(200).min(text.len()));
                text.get(start..end).unwrap_or_default().to_owned()
            };
            let message = format!(
                "a component at rest skipped a sync that changed what it shows ({}): \
                 record moved {}, layout dirty {} -> {}, asked of its host (controls, \
                 searches, replacement) {:?} -> {:?}, semantics were `{}`, are `{}`",
                self.component.callback_binding().app_id,
                before.1 != after.1,
                before.2,
                after.2,
                before.3,
                after.3,
                excerpt(&before.0),
                excerpt(&after.0),
            );
            assert!(
                consistent || !cfg!(any(test, feature = "test-support")),
                "{message}"
            );
            tracing::error!("{message}");
        }

        /// Whether the root still works through its passes: what its record's store owes, motion, a
        /// presentation still settling (an image decoding).
        fn busy(root: &dyn AppWidget) -> bool {
            root.needs_idle_sync() || root.needs_animation_frame() || root.has_pending_visual_work()
        }

        /// Note what the pass that just ran read: `reads` outside the component, and what stands now.
        fn observation_pass(
            &mut self,
            outer: &AutoCommit,
            reads: tilo_ui_base::component_inputs::Reads,
        ) {
            if self.in_root(|root| Self::busy(root)).unwrap_or(true) {
                self.observation = None;
                return;
            }
            let now = self.inputs_now(outer);
            let reads = match self.update_reads.take() {
                Some(update) => reads.join(update),
                None => reads,
            };
            let reads = match self.observation.take() {
                Some(previous) if self.owes_settle => previous.reads.join(reads),
                Some(previous) => reads.carry(previous.reads),
                None => reads,
            };
            // The language that stands now: a pass words loaded while it ran
            // read in two languages, and proves nothing (see `PassReading`),
            // so the next pass shows them in the one that stands.
            let locale = now.inputs.locale;
            self.observation = match (now.revision, now.document) {
                (Some(revision), Some(document)) => Some(Observation {
                    revision,
                    document,
                    inputs: PassInputs {
                        owner: crate::bound_dispatch::OwnerInputs {
                            save: self
                                .component
                                .sampled_resource_save()
                                .cloned()
                                .ok_or_else(String::new),
                            ..now.inputs.owner
                        },
                        locale,
                    },
                    reads,
                }),
                _ => None,
            };
        }

        /// Run `f` on the component's root inside the component's record frame, lending it the frame's
        /// edits at the record root.
        fn with_root_edits<R: Default>(
            &mut self,
            edits: &mut tilo_ui_base::widget::Edits<'_>,
            f: impl FnOnce(
                &mut (dyn Widget + 'static),
                &mut tilo_ui_base::widget::Edits<'_>,
                &ObjId,
            ) -> R,
        ) -> R {
            let binding = self.component.callback_binding();
            binding.note_input();
            binding
                .with_document(edits.document_mut(), |document, resource_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return R::default();
                    };
                    f(
                        root,
                        &mut tilo_ui_base::widget::Edits::lend(document),
                        resource_root,
                    )
                })
                .unwrap_or_default()
        }

        /// Run `f` on the component's root inside its callback binding.
        fn in_root<'a, R>(
            &'a self,
            f: impl FnOnce(&'a (dyn AppWidget + 'static)) -> R,
        ) -> Option<R> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref().map(f)
        }

        /// Whether the record's last sampled save is still unconfirmed.
        fn saving(&self) -> bool {
            let binding = self.component.callback_binding();
            binding.enter().is_ok() && self.component.record_saving()
        }

        /// [`Self::in_root`] with the root lent mutably.
        fn in_root_mut<R>(
            &mut self,
            f: impl FnOnce(&mut (dyn AppWidget + 'static)) -> R,
        ) -> Option<R> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref_mut().map(f)
        }
    }

    #[cfg(feature = "model")]
    impl tilo_ui_base::widget::Continuable for BoundAppWidget {
        fn continuation_metadata(
            &self,
            state: &AutoCommit,
            _path: &ObjId,
        ) -> Option<::collaboration::ContinuationMetadata> {
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, resource_root| {
                    let root = self.component.root.as_deref()?;
                    tilo_ui_base::widget::capability::capability::<
                        dyn tilo_ui_base::widget::Continuable,
                    >(root)?
                    .continuation_metadata(document, resource_root)
                })
                .unwrap_or(None)
        }

        fn restore_continuation_metadata(
            &mut self,
            metadata: &::collaboration::ContinuationMetadata,
            state: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &ObjId,
        ) -> bool {
            self.with_root_edits(state, |root, edits, resource_root| {
                let Some(continuable) = tilo_ui_base::widget::capability::capability_mut::<
                    dyn tilo_ui_base::widget::Continuable,
                >(&mut *root) else {
                    return false;
                };
                continuable.restore_continuation_metadata(metadata, edits, resource_root)
            })
        }
    }

    impl FloatingSurfaceProvider for BoundAppWidget {
        fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_floating_surface_ime_source_changed(id));
        }

        fn draw_floating_surface(
            &mut self,
            id: &str,
            ctx: &mut tilo_ui_base::draw::DrawCtx<'_>,
        ) -> bool {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
            self.in_root_mut(|root| {
                root.draw_floating_surface(
                    id,
                    &mut tilo_ui_base::draw::DrawCtx::new(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        env.clone(),
                    ),
                )
            })
            .unwrap_or_default()
        }

        fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_floating_surface_focus_changed(id, focused));
        }

        fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_floating_surface_event_source(id));
        }

        fn floating_surface_event(
            &mut self,
            id: &str,
            event: &winit::event::WindowEvent,
            ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
        ) -> bool {
            self.owes_sync = true;
            self.in_root_mut(|root| root.floating_surface_event(id, event, ctx))
                .unwrap_or_default()
        }

        fn floating_surface_accessibility(
            &self,
            id: &str,
        ) -> Option<tilo_ui_base::a11y::AccessNode> {
            self.in_root(|root| root.floating_surface_accessibility(id))
                .flatten()
        }

        fn perform_floating_surface_accessibility_action(
            &mut self,
            id: &str,
            target: &str,
            action: &tilo_ui_base::a11y::Action,
            state: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &ObjId,
        ) -> bool {
            self.with_root_edits(state, |root, edits, resource_root| {
                root.perform_floating_surface_accessibility_action(
                    id,
                    target,
                    action,
                    edits,
                    resource_root,
                )
            })
        }

        fn floating_surface_ime(
            &self,
            id: &str,
        ) -> Option<(tilo_ui_base::ime::InputContent, vello::kurbo::Rect)> {
            self.in_root(|root| root.floating_surface_ime(id)).flatten()
        }

        fn take_floating_surface_request(
            &mut self,
        ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
            self.in_root_mut(|root| root.take_floating_surface_request())
                .flatten()
        }

        fn take_pip_area_request(&mut self) -> bool {
            self.in_root_mut(|root| root.take_pip_area_request())
                .unwrap_or(false)
        }

        fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
            self.in_root_mut(FloatingSurfaceWidgetExt::take_floating_surface_source_updates)
                .unwrap_or_default()
        }

        fn draw_floating_surfaces(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
            self.in_root_mut(|root| {
                root.draw_floating_surfaces(&mut tilo_ui_base::draw::DrawCtx::new(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    env,
                ))
            });
        }

        fn floating_surface_presentations(
            &self,
        ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
            self.in_root(FloatingSurfaceWidgetExt::floating_surface_presentations)
                .unwrap_or_default()
        }

        fn active_floating_surface_ids(&self) -> Vec<String> {
            self.in_root(FloatingSurfaceWidgetExt::active_floating_surface_ids)
                .unwrap_or_default()
        }

        fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect) {
            self.in_root_mut(|root| root.set_floating_surface_bounds(bounds));
        }

        fn on_floating_surface_host_message(
            &mut self,
            message: &crate::floating_surface::FloatingSurfaceHostMessage,
        ) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_floating_surface_host_message(message));
        }
    }

    impl AudiencePresentationProvider for BoundAppWidget {
        fn audience_presentation_request(
            &self,
        ) -> Option<tilo_ui_base::fullscreen::AudiencePresentationRequest> {
            self.in_root(|root| root.audience_presentation_request())
                .flatten()
        }

        fn on_audience_presentation_changed(&mut self, active: bool) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_audience_presentation_changed(active));
        }

        fn draw_audience_presentation(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            rect: vello::kurbo::Rect,
        ) -> bool {
            self.in_root_mut(|root| {
                root.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
            })
            .unwrap_or_default()
        }

        fn handle_audience_presentation_event(
            &mut self,
            event: &WindowEvent,
            state: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &ObjId,
        ) -> bool {
            self.owes_sync = true;
            self.with_root_edits(state, |root, edits, resource_root| {
                root.handle_audience_presentation_event(event, edits, resource_root)
            })
        }

        fn on_audience_presentation_closed(&mut self) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_audience_presentation_closed());
        }
    }

    impl FullscreenProvider for BoundAppWidget {
        fn fullscreen_intent(&self) -> Option<bool> {
            self.in_root(|root| root.fullscreen_intent()).flatten()
        }

        fn fullscreen_params(&self) -> tilo_ui_base::fullscreen::FullscreenRequest {
            self.in_root(FullscreenWidgetExt::fullscreen_params)
                .unwrap_or_else(|| {
                    tilo_ui_base::fullscreen::FullscreenRequest::mirror(
                        tilo_ui_base::fullscreen::FullscreenTarget::Largest,
                    )
                })
        }

        fn on_fullscreen_changed(&mut self, active: bool) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.on_fullscreen_changed(active));
        }
    }

    impl CompleteContentProvider for BoundAppWidget {
        /// The root's own complete content, else a scrolling document it shows at full height: the window
        /// taller by what the document's primary scroll hides.
        fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
            self.in_root(|root| {
                if let Some(size) = root.complete_content_size() {
                    return Some(size);
                }
                let hidden = unrolled_height(root)?;
                let rect = root.get_rect();
                Some(vello::kurbo::Size::new(
                    rect.width(),
                    rect.height() + hidden,
                ))
            })
            .flatten()
        }

        fn draw_complete_content(
            &mut self,
            bounds: vello::kurbo::Rect,
            ctx: &mut tilo_ui_base::draw::DrawCtx<'_>,
        ) -> bool {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
            if self.component.behaviour.capture_protected {
                tilo_ui_base::secret_paint::note_secret_painted();
            }
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return false;
            };
            if root.complete_content_size().is_some() {
                return root.draw_complete_content(
                    bounds,
                    &mut tilo_ui_base::draw::DrawCtx::new(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        env.clone(),
                    ),
                );
            }
            if unrolled_height(root).is_none() || bounds.area() <= 0.0 {
                return false;
            }
            let live = root.get_rect();
            let offset = root
                .primary_vertical_scroll()
                .map(|region| region.metrics.offset);
            if tilo_ui_base::widget::layout::lay_out_detached(root, bounds, theme, text_renderer)
                .is_err()
            {
                return false;
            }
            let inner = crate::widget::app_widget::lent_permissions(&env, &binding);
            root.draw_child(scene, transform, text_renderer, theme, &inner);
            let _ =
                tilo_ui_base::widget::layout::lay_out_detached(root, live, theme, text_renderer);
            if let Some(offset) = offset {
                let _ = root.apply_primary_vertical_scroll(
                    tilo_ui_base::widget::PrimaryScrollAction::Absolute(offset),
                );
            }
            self.relayout = true;
            true
        }
    }

    /// How much of the root's document its primary vertical scroll hides.
    fn unrolled_height(root: &dyn Widget) -> Option<f64> {
        let metrics = root.primary_vertical_scroll()?.metrics;
        let hidden = metrics.content - metrics.viewport;
        (hidden.is_finite() && hidden > 1.0).then_some(hidden)
    }

    impl Widget for BoundAppWidget {
        tilo_ui_base::provides!(
            dyn crate::AppWidget,
            dyn tilo_ui_base::widget::FullscreenProvider,
            dyn tilo_ui_base::widget::CompleteContentProvider,
            #[cfg(feature = "model")]
            dyn tilo_ui_base::widget::Continuable,
            dyn crate::floating_surface::FloatingSurfaceProvider,
            dyn tilo_ui_base::widget::AudiencePresentationProvider,
            #[cfg(feature = "model")]
            dyn tilo_ui_base::widget::presence_topology::Presences,
            dyn tilo_ui_base::widget::undo_access::CommittedActions,
            dyn tilo_ui_base::widget::undo_access::UndoControl,
            dyn tilo_ui_base::widget::undo_access::LocalUndo,
            #[cfg(feature = "model")]
            dyn crate::history_mode::capability::DocumentHistory,
            dyn tilo_ui_base::widget::controls::ActionControls,
            dyn tilo_ui_base::widget::resources::PublishesResources,
            dyn crate::search::capability::SearchSource,
            #[cfg(feature = "model")]
            dyn tilo_ui_base::widget::permission::PermissionGuarded,
            dyn tilo_ui_base::presence::capability::PublishesPresence,
            #[cfg(feature = "model")]
            dyn crate::resource::capability::ResourceScoped,
            dyn crate::view::capability::ViewState,
            dyn crate::component_lifecycle::ComponentLifecycle,
            dyn crate::component_lifecycle::WindowActivation,
            dyn crate::component_lifecycle::WindowTitle,
            #[cfg(feature = "model")]
            dyn crate::component_lifecycle::HearsElsewhere,
            dyn crate::widget::WindowHistory,
            dyn tilo_ui_base::widget::undo_points::UndoPoints,
            dyn tilo_ui_base::widget::ResourceOperations,
            dyn crate::bound_dispatch::BoundWidget,
            dyn tilo_ui_base::widget::component_walk::ComponentBoundary,
            dyn crate::bound_dispatch::BoundTraversal
        );
        fn as_any(&self) -> Option<&dyn std::any::Any> {
            Some(self)
        }

        fn observe_physical_input(&mut self, input: tilo_ui_base::widget::PhysicalInput) {
            self.in_root_mut(|root| root.observe_physical_input(input));
        }

        fn paints_ground(&self) -> bool {
            self.in_root(|root| root.paints_ground()).unwrap_or(true)
        }

        fn native_input_snapshot(
            &self,
            _: &tilo_ui_base::widget::AppModel,
            _: &tilo_ui_base::widget::ModelPath,
        ) -> Option<serde_json::Value> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            let root = self.component.root.as_deref()?;
            binding
                .records
                .read(&binding.checkout.id, |doc| {
                    root.native_input_snapshot(doc, &automerge::ROOT)
                })
                .flatten()
        }

        fn view_only_scroll(&self, event: &WindowEvent) -> bool {
            self.in_root(|root| root.view_only_scroll(event))
                .unwrap_or_default()
        }

        fn primary_vertical_scroll(&self) -> Option<tilo_ui_base::widget::PrimaryScrollRegion> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            let mut region = self.component.root.as_deref()?.primary_vertical_scroll()?;
            region.owner_generation = Some(binding.lifetime_id);
            Some(region)
        }
        fn apply_primary_vertical_scroll(
            &mut self,
            action: tilo_ui_base::widget::PrimaryScrollAction,
        ) -> tilo_ui_base::widget::PrimaryScrollResult {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return tilo_ui_base::widget::PrimaryScrollResult::Unsupported;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return tilo_ui_base::widget::PrimaryScrollResult::Unsupported;
            };
            root.apply_primary_vertical_scroll(action)
                .stamp(binding.lifetime_id)
        }

        fn mount_native_views(
            &mut self,
            host: &tilo_ui_base::widget::native_view::NativeViewHost<'_>,
        ) {
            self.in_root_mut(|root| root.mount_native_views(host));
        }
        fn unmount_native_views(&mut self) {
            self.in_root_mut(|root| root.unmount_native_views());
        }
        fn native_view_frame_start(&mut self) {
            self.in_root_mut(|root| root.native_view_frame_start());
        }
        fn set_platform_conventions(
            &mut self,
            conventions: tilo_ui_base::platform::PlatformConventions,
        ) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.set_platform_conventions(conventions));
        }
        fn set_interaction_environment(
            &mut self,
            environment: tilo_ui_base::interaction::InteractionEnvironment,
        ) {
            if self.environment != Some(environment) {
                self.environment = Some(environment);
                self.owes_sync = true;
            }
            self.in_root_mut(|root| root.set_interaction_environment(environment));
        }
        fn sync(
            &mut self,
            state: &tilo_ui_base::widget::AppModel,
            path: &tilo_ui_base::widget::ModelPath,
        ) {
            let (state, _) = tilo_ui_base::doc::document_ref(state, path);
            if self.at_rest(state) {
                #[cfg(debug_assertions)]
                self.verify_rest(state);
                return;
            }
            let _profile = tilo_actions::profiling::Phase::start("ui.component.sync");
            let binding = self.component.callback_binding();
            let ((), reads) = crate::resource::reads::reading(&binding.lifetime, || {
                self.sync_root(state);
            });
            self.owes_sync = false;
            self.observation_pass(state, reads);
            self.owes_settle = true;
        }
        fn settle(&mut self, edits: &mut tilo_ui_base::widget::Edits<'_>, _path: &ObjId) {
            // Owed by a sync of this frame or the last: what it writes is
            // what that sync took up, and the observation follows it, unless
            // something reached the component since that sync, which only
            // the next sync takes up.
            let owed = self.owes_settle
                && self.observation.as_ref().is_some_and(|observation| {
                    observation
                        .moved(&self.inputs_now(edits.document_mut()))
                        .is_none()
                });
            if !owed && self.at_rest(edits.document_mut()) {
                return;
            }
            let binding = self.component.callback_binding();
            let ((), reads) = crate::resource::reads::reading(&binding.lifetime, || {
                self.settle_root(edits);
            });
            // What the settle wrote into a document the window keeps beside
            // its own (a chain's part) is sampled here: the frame that wrote
            // it shows its save, and a save it still owes is registered with
            // its owner, so landing it wakes the window.
            let _ = self.component.project_resource_save_status();
            if owed {
                self.observation_pass(edits.document_mut(), reads);
            }
            self.owes_settle = false;
        }
        fn update(
            &mut self,
            state: &tilo_ui_base::widget::AppModel,
            path: &tilo_ui_base::widget::ModelPath,
        ) -> bool {
            let (state, _) = tilo_ui_base::doc::document_ref(state, path);
            let binding = self.component.callback_binding();
            let permissions = binding.permissions.borrow().clone();
            let update = |component: &mut AppComponent| {
                if binding.records.is_opening(&binding.checkout.id) {
                    return component
                        .root
                        .as_deref_mut()
                        .is_some_and(|root| root.update(state, &automerge::ROOT));
                }
                binding
                    .read_document(state, |document, resource_root| {
                        component
                            .root
                            .as_deref_mut()
                            .is_some_and(|root| root.update(document, resource_root))
                    })
                    .unwrap_or(false)
            };
            let component = &mut self.component;
            let (changed, reads) = crate::resource::reads::reading(&binding.lifetime, || {
                tilo_ui_base::presence::frame::frame(|presence| {
                    presence.with_permissions(permissions.clone(), || update(component))
                })
                .unwrap_or_else(|| update(component))
            });
            self.update_reads = Some(match self.update_reads.take() {
                Some(earlier) => earlier.join(reads),
                None => reads,
            });
            if changed {
                self.owes_sync = true;
                binding.note_input();
            }
            changed
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            // A control the build makes is a change of the root's own
            // ([`Self::take_structure_change`]), which owes the sync that
            // feeds it before it is measured: laying out a window whose tree
            // stood as it was reads nothing new.
            let Some(child) = self.in_root_mut(|root| root.build_layout(taffy, theme, node_map))
            else {
                return taffy.leaf_node(taffy::Style::default());
            };
            let style = taffy::Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Default::default()
            };
            taffy.widget_node(style, &[child], node_map, self)
        }
        fn measure(
            &mut self,
            known: taffy::Size<Option<f32>>,
            available: taffy::Size<taffy::AvailableSpace>,
            text_renderer: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            self.in_root_mut(|root| root.measure(known, available, text_renderer))
                .unwrap_or_default()
        }
        fn apply_layout(
            &mut self,
            taffy: &TaffyTree<()>,
            node: NodeId,
            origin: vello::kurbo::Point,
        ) {
            let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
            if self.laid_out != Some(rect.size()) {
                self.laid_out = Some(rect.size());
                self.owes_sync = true;
            }
            self.in_root_mut(|root| {
                if let Ok(children) = taffy.children(node)
                    && let Some(child) = children.first()
                {
                    root.apply_layout(taffy, *child, rect.origin());
                }
            });
        }
        fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return;
            };
            let Some(root) = self.component.root.as_deref_mut() else {
                return;
            };
            // Protection belongs to this component even inside a shell or
            // chrome container; its enclosing presented frame must retain it.
            if self.component.behaviour.capture_protected {
                tilo_ui_base::secret_paint::note_secret_painted();
            }
            let inner = crate::widget::app_widget::lent_permissions(&env, &binding);
            root.draw_child(scene, transform, text_renderer, theme, &inner);
        }
        fn handle_event(
            &mut self,
            event: &WindowEvent,
            ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
        ) {
            let binding = self.component.callback_binding();
            // Whether this event alone panned a view: the host reads the
            // same answer once dispatch ends, so it is handed back.
            let panned_before = tilo_ui_base::widget::take_view_only_pan();
            let declared_pan = self
                .component
                .root
                .as_deref()
                .is_some_and(|root| root.view_only_scroll(event));
            binding
                .with_document(ctx.state.document_mut(), |document, resource_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return;
                    };
                    if let Ok(children) = ctx.taffy.children(ctx.node_id)
                        && let Some(child) = children.first()
                    {
                        root.handle_event(
                            event,
                            &mut tilo_ui_base::widget::EventCtx {
                                scale_factor: ctx.scale_factor,
                                modifiers: ctx.modifiers,
                                state: tilo_ui_base::widget::Edits::lend(document),
                                path: resource_root,
                                handled: ctx.handled,
                                focus_target: ctx.focus_target,
                                node_id: *child,
                                taffy: ctx.taffy,
                            },
                        );
                    }
                    let _ = binding.drain_owned_tree(root, document);
                })
                .unwrap_or(());
            // An event a widget took is input the next sync takes up. One
            // nobody took changed nothing a sync reads but what says so
            // itself: its record, its local state.
            //
            // A wheel that only moved a scroller's view (the root declares
            // it, or the scroller says so) shows through the scroller alone;
            // an edit it made moves the record, focus it moved is input of
            // its own.
            let panned = tilo_ui_base::widget::take_view_only_pan();
            if panned || panned_before {
                tilo_ui_base::widget::note_view_only_pan();
            }
            if *ctx.handled && !(panned || declared_pan) {
                binding.note_input();
            }
        }
        fn children(&self) -> Vec<&dyn Widget> {
            self.component
                .root
                .as_deref()
                .map_or_else(Vec::new, |root| vec![root as &dyn Widget])
        }
        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
            if let Some(root) = self.component.root.as_deref() {
                visitor(root);
            }
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            self.component
                .root
                .as_deref_mut()
                .map_or_else(Vec::new, |root| vec![root as &mut dyn Widget])
        }
        fn name(&self) -> &'static str {
            "BoundComponent"
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
        fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
            self.in_root(|root| root.test_rects()).unwrap_or_default()
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.in_root(|root| root.get_rect()).unwrap_or_default()
        }
        fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
            self.in_root(|root| root.cursor_shape(point)).flatten()
        }
        fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
            self.in_root(|root| root.tooltip()).flatten()
        }
        fn set_focus(&mut self, focused: bool) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.set_focus(focused));
        }
        fn wants_tab_input(&self) -> bool {
            self.in_root(|root| root.wants_tab_input())
                .unwrap_or_default()
        }
        fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
            self.in_root(|root| root.ime_cursor_rect()).flatten()
        }
        fn traps_focus(&self) -> bool {
            self.in_root(|root| root.traps_focus()).unwrap_or_default()
        }
        fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<vello::kurbo::Rect> {
            self.in_root(|root| root.synthetic_accessibility_rect(path))
                .flatten()
        }
        fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
            self.owes_sync = true;
            self.in_root_mut(|root| root.focus_synthetic_accessibility(path))
                .unwrap_or_default()
        }
        fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
            self.in_root(|root| root.accessibility_child_parent(child_index))
                .flatten()
        }
        fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
            self.in_root(|root| root.accessibility_child_path(child_index))
                .flatten()
        }
        fn accessibility_child_node(&self, child_index: usize) -> Option<Vec<usize>> {
            self.in_root(|root| root.accessibility_child_node(child_index))
                .flatten()
        }
        fn accessibility_represents_child(&self, child_index: usize) -> bool {
            self.in_root(|root| root.accessibility_represents_child(child_index))
                .unwrap_or_default()
        }
        fn lays_out_child(&self, child_index: usize) -> bool {
            self.in_root(|root| root.lays_out_child(child_index))
                .unwrap_or_default()
        }
        fn accessibility_child_description(&self, child_index: usize) -> Option<String> {
            self.in_root(|root| root.accessibility_child_description(child_index))
                .flatten()
        }
        fn accessibility_child_name(&self, child_index: usize) -> Option<String> {
            self.in_root(|root| root.accessibility_child_name(child_index))
                .flatten()
        }
        fn child_path_name(&self, child_index: usize) -> Option<String> {
            self.in_root(|root| root.child_path_name(child_index))
                .flatten()
        }
        fn perform_action(
            &mut self,
            action: &tilo_ui_base::a11y::Action,
            key_path: &[u64],
            state: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &tilo_ui_base::widget::ModelPath,
        ) -> bool {
            self.with_root_edits(state, |root, edits, resource_root| {
                root.perform_action(action, key_path, edits, resource_root)
            })
        }
        fn perform_semantic_action(
            &mut self,
            target: &str,
            action: &tilo_ui_base::a11y::Action,
            state: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &tilo_ui_base::widget::ModelPath,
        ) -> bool {
            self.with_root_edits(state, |root, edits, resource_root| {
                root.perform_semantic_action(target, action, edits, resource_root)
            })
        }
        fn is_enabled(&self) -> bool {
            self.in_root(|root| root.is_enabled()).unwrap_or_default()
        }
        fn ime_requires_deferred_ack(&self) -> bool {
            self.in_root(|root| root.ime_requires_deferred_ack())
                .unwrap_or(true)
        }

        fn ime_refuses_ordinary_commits(&self) -> bool {
            self.in_root(|root| root.ime_refuses_ordinary_commits())
                .unwrap_or(true)
        }

        fn start_deferred_text(&mut self) -> Option<tilo_webview::text_input::Receipt> {
            self.owes_sync = true;
            self.in_root_mut(|root| root.start_deferred_text())
                .flatten()
        }

        fn commit_deferred_text(
            &mut self,
            text: &str,
        ) -> Option<tilo_webview::text_input::Receipt> {
            self.owes_sync = true;
            self.in_root_mut(|root| root.commit_deferred_text(text))
                .flatten()
        }

        fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
            self.in_root(|root| root.ime_edit_fingerprint(key))
                .flatten()
        }
        fn ime_content(&self) -> Option<tilo_ui_base::ime::InputContent> {
            self.in_root(|root| root.ime_content()).flatten()
        }
        fn ime_multiline(&self) -> bool {
            self.in_root(Widget::ime_multiline).unwrap_or_default()
        }

        fn presented(&self) -> bool {
            self.in_root(|root| root.presented()).unwrap_or_default()
        }
        fn arrives_from(&self) -> tilo_ui_base::arriving::Source {
            self.in_root(|root| root.arrives_from())
                .unwrap_or(tilo_ui_base::arriving::Source::Local)
        }
        fn arrival_progress(&self) -> Option<tilo_ui_base::arriving::Progress> {
            self.in_root(|root| root.arrival_progress()).flatten()
        }
        fn arrival_shape(&self) -> tilo_ui_base::widget::SkeletonShape {
            self.in_root(|root| root.arrival_shape())
                .unwrap_or(tilo_ui_base::widget::SkeletonShape::Ground)
        }
        fn text_review(&self) -> Option<(usize, String)> {
            self.in_root(|root| root.text_review()).flatten()
        }
        fn nav_availability(&self) -> tilo_ui_base::gesture::NavAvailability {
            self.in_root(Widget::nav_availability).unwrap_or_default()
        }
        fn on_external_event(
            &mut self,
            payload: &str,
            state: &mut tilo_ui_base::widget::Edits<'_>,
            _root: &tilo_ui_base::widget::ModelPath,
        ) {
            self.with_root_edits(state, |root, edits, resource_root| {
                root.on_external_event(payload, edits, resource_root);
            });
        }
        fn on_nav_gesture(
            &mut self,
            dir: tilo_ui_base::gesture::NavDirection,
            state: &tilo_ui_base::widget::AppModel,
            path: &tilo_ui_base::widget::ModelPath,
        ) -> bool {
            let (state, _) = tilo_ui_base::doc::document_ref(state, path);
            self.owes_sync = true;
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, resource_root| {
                    self.component
                        .root
                        .as_deref_mut()
                        .is_some_and(|root| root.on_nav_gesture(dir, document, resource_root))
                })
                .unwrap_or(false)
        }
        fn take_focus_reset(&mut self) -> bool {
            self.in_root_mut(|root| root.take_focus_reset())
                .unwrap_or_default()
        }
        fn appearance_intent(&self) -> Option<tilo_ui_base::theme::AppearanceIntent> {
            self.in_root(|root| root.appearance_intent()).flatten()
        }
        fn set_motion_reduced(&mut self, reduced: bool) {
            self.owes_sync = true;
            self.in_root_mut(|root| root.set_motion_reduced(reduced));
        }
        fn needs_idle_sync(&self) -> bool {
            self.in_root(|root| root.needs_idle_sync())
                .unwrap_or_default()
        }

        fn wants_pointer_capture(&self) -> bool {
            self.in_root(|root| root.wants_pointer_capture())
                .unwrap_or_default()
        }
        fn needs_animation_frame(&self) -> bool {
            self.in_root(|root| root.needs_animation_frame())
                .unwrap_or_default()
        }
        /// A save the store has not confirmed yet is pending too: "Saving…" is a placeholder for "Saved",
        /// so a still capture waits for it.
        fn has_pending_visual_work(&self) -> bool {
            self.in_root(Widget::has_pending_visual_work)
                .unwrap_or_default()
                || self.saving()
        }
        fn has_transient_feedback(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            self.component.record_saving()
                || self
                    .component
                    .root
                    .as_deref()
                    .is_some_and(Widget::has_transient_feedback)
        }
        fn serve_deadline(&mut self, now: Instant) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return true;
            };
            let served = self
                .component
                .root
                .as_deref_mut()
                .is_none_or(|root| root.serve_deadline(now));
            if served {
                self.owes_sync = true;
                binding.note_input();
            }
            served
        }
        fn next_wake_deadline(&self) -> Option<Instant> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            self.component.root.as_deref()?.next_wake_deadline()
        }
        fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
            self.in_root(|root| root.modal_capture()).flatten()
        }
        fn take_structure_dirty(&mut self) -> bool {
            self.take_structure_change().rebuilds()
        }
        /// The root's change as it says it, and a relayout the component asked for, which rebuilds.
        fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
            use tilo_ui_base::widget::StructureChange;
            let relayout = StructureChange::of(std::mem::take(&mut self.relayout));
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return relayout;
            };
            if self.component.take_save_status_changed() {
                self.save_status_shown = true;
            }
            let Some(root) = self.component.root.as_deref_mut() else {
                return relayout;
            };
            let change = root.take_structure_change().join(relayout);
            // A change the widgets carry out in place leaves every widget
            // alive: what they mount they build from what they hold. A tree
            // built anew may stand with controls it made, which take what
            // they show in the sync that precedes their measure.
            if matches!(
                change,
                StructureChange::Described | StructureChange::Rebuild
            ) {
                self.owes_sync = true;
            }
            change
        }
        /// The component's root carries the change below it; this wrapper only keeps the root's node.
        fn remount_children(&mut self, remount: &mut tilo_ui_base::widget::Remount<'_>) -> bool {
            self.in_root_mut(|root| root.remount_children(remount))
                .unwrap_or(true)
        }
        fn needs_redraw(&self) -> bool {
            self.save_status_shown
                || self
                    .in_root(|root| root.needs_redraw() || root.needs_animation_frame())
                    .unwrap_or_default()
        }
        fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
            self.in_root(|root| root.collect_damage(regions));
        }
        fn mark_drawn(&mut self) {
            self.save_status_shown = false;
            self.in_root_mut(|root| root.mark_drawn());
        }
        fn release_scene_cache(&mut self) {
            self.in_root_mut(|root| root.release_scene_cache());
        }
        fn is_layout_dirty(&self) -> bool {
            self.in_root(|root| root.is_layout_dirty())
                .unwrap_or_default()
        }
        fn layout_needs_measure(&self) -> bool {
            self.in_root(|root| root.layout_needs_measure())
                .unwrap_or_default()
        }
        fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
            if self.component.behaviour.capture_protected {
                tilo_ui_base::secret_paint::note_secret_painted();
            }
            self.in_root_mut(|root| {
                root.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    env,
                ))
            });
        }
    }

    impl crate::bound_dispatch::BoundWidget for BoundAppWidget {}

    impl tilo_ui_base::widget::component_walk::ComponentBoundary for BoundAppWidget {
        fn component_scope(&self) -> Option<tilo_ui_base::widget::component_walk::Binding> {
            Some(std::rc::Rc::new(self.component.callback_binding()))
        }
    }

    impl crate::bound_dispatch::BoundTraversal for BoundAppWidget {
        // Local feedback belongs to the emitting component. The host delivers
        // bound results through with_live_component; generic tree recursion
        // must neither cross into another record nor revive a closed owner.
        fn visit_components(
            &mut self,
            request: &mut crate::bound_dispatch::ComponentRequest<'_>,
        ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
            let binding = self.component.callback_binding();
            if binding.closing.get() {
                return Ok(());
            }
            let Ok(_entry) = binding.lifetime.enter_if_open() else {
                return Ok(());
            };
            if request
                .target
                .is_none_or(|target| target.same_component(&binding))
            {
                if request.save_progress.is_some() && request.target.is_some() {
                    request.save_progress = Some(self.component.project_resource_save_status());
                    request.found = true;
                    return Ok(());
                }
                let target = request.target.unwrap_or(&binding);
                if let Some((invocation, accepted)) = self
                    .component
                    .with_live_component(target, &mut *request.visit)?
                {
                    if accepted {
                        if !self.component.invocations.contains(&invocation) {
                            self.component.invocations.push(invocation);
                        }
                    } else {
                        self.component.invocations.retain(|id| *id != invocation);
                    }
                }
                request.found = true;
                if request.target.is_some() {
                    return Ok(());
                }
            }
            if let Some(root) = self.component.root.as_deref_mut() {
                crate::bound_dispatch::visit_components(root, request)?;
            }
            Ok(())
        }
    }

    impl tilo_ui_base::widget::ResourceOperations for BoundAppWidget {
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            self.in_root(|root| tilo_ui_base::widget::local_resource_operation_handlers(root))
                .unwrap_or(&[])
        }

        fn resource_operation_handlers(&self) -> HashSet<String> {
            self.in_root(|root| tilo_ui_base::widget::resource_operation_handlers(root))
                .unwrap_or_default()
        }

        fn start_operation(
            &mut self,
            op: &tilo_ui_base::widget::ResourceOperation<'_>,
            doc: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            let binding = self.component.callback_binding();
            binding.note_input();
            let result = binding
                .with_document(doc.document_mut(), |document, resource_root| {
                    let Some(root) = self.component.root.as_deref_mut() else {
                        return Err("component callback refused".into());
                    };
                    tilo_ui_base::widget::start_operation(
                        &mut *root,
                        op,
                        &mut tilo_ui_base::widget::Edits::lend(document),
                        resource_root,
                    )
                })
                .unwrap_or_else(|_| Err("component callback refused".into()));
            if matches!(result, Ok(Some(ResourceOperationStart::Accepted)))
                && !self.component.invocations.contains(&op.invocation())
            {
                self.component.invocations.push(op.invocation());
            }
            result
        }

        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            cancellation_requested: bool,
            doc: &AutoCommit,
            _path: &ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            let binding = self.component.callback_binding();
            let update = binding
                .read_document(doc, |document, resource_root| {
                    let root = self.component.root.as_deref_mut()?;
                    tilo_ui_base::widget::poll_resource_operation(
                        &mut *root,
                        invocation,
                        cancellation_requested,
                        document,
                        resource_root,
                    )
                })
                .unwrap_or(None);
            if update
                .as_ref()
                .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.component.invocations.retain(|id| *id != invocation);
            }
            update
        }
    }

    impl tilo_ui_base::widget::undo_points::UndoPoints for BoundAppWidget {
        fn get_state(&self) -> Option<String> {
            self.in_root(|root| tilo_ui_base::widget::undo_points::get_state(root))
                .flatten()
        }
        fn set_state(&mut self, state: &str) {
            self.in_root_mut(|root| tilo_ui_base::widget::undo_points::set_state(root, state));
        }
    }

    impl crate::component_lifecycle::WindowActivation for BoundAppWidget {
        fn take_window_activation_intent(&self) -> bool {
            self.in_root(crate::component_lifecycle::take_window_activation_intent)
                .unwrap_or_default()
        }
    }

    /// What the component the window shows says it shows, for the window's own title: the app's root
    /// answers it, so a window whose content names the page it is at is named by that.
    /// The History the component's root offers its window: what a window whose app runs elsewhere
    /// (a tilolang host's instance) shows of the history of what it shows.
    impl crate::widget::WindowHistory for BoundAppWidget {
        fn set_window_presents_history(&mut self, tab_presents: bool) {
            self.in_root_mut(|root| {
                if let Some(history) = tilo_ui_base::widget::capability::capability_mut::<
                    dyn crate::widget::WindowHistory,
                >(root)
                {
                    history.set_window_presents_history(tab_presents);
                }
            });
        }

        fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
            self.in_root(|root| {
                tilo_ui_base::widget::capability::capability::<dyn crate::widget::WindowHistory>(
                    root,
                )?
                .window_history_control()
            })
            .flatten()
        }

        fn invoke_window_history_control(
            &mut self,
            control: crate::widget::HistoryControl,
        ) -> Result<(), String> {
            self.in_root_mut(|root| {
                tilo_ui_base::widget::capability::capability_mut::<dyn crate::widget::WindowHistory>(root)
                    .ok_or("History is unavailable")?
                    .invoke_window_history_control(control)
            })
            .unwrap_or_else(|| Err("History is unavailable".to_owned()))
        }
    }

    impl crate::component_lifecycle::WindowTitle for BoundAppWidget {
        fn window_title(&self) -> Option<&str> {
            self.in_root(crate::component_lifecycle::window_title)
                .flatten()
        }
    }

    /// What the connection of the component's app heard elsewhere (the app's instance in its own
    /// process) of the people at it: the app's root hands it over.
    #[cfg(feature = "model")]
    impl crate::component_lifecycle::HearsElsewhere for BoundAppWidget {
        fn take_heard(&mut self) -> Vec<crate::component_lifecycle::Heard> {
            self.in_root_mut(|root| crate::component_lifecycle::take_heard(root))
                .unwrap_or_default()
        }
    }

    impl crate::component_lifecycle::ComponentLifecycle for BoundAppWidget {
        fn unsaved_documents(
            &self,
            _: &AutoCommit,
        ) -> Vec<crate::close_admission::UnsavedDocument> {
            self.component.unsaved_documents()
        }
        fn checkpoint_components(&mut self) -> Vec<String> {
            self.component.checkpoint_for_close()
        }
        fn close_settling(&mut self) -> crate::close_admission::Settling {
            self.component.close_settling()
        }
        fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            self.component.finish_admitted_close();
            self.component.take_close_reports()
        }
        fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
            self.component.close();
            self.component.take_close_reports()
        }
        fn take_replacement(&mut self) -> Option<crate::application::ComponentReplacement> {
            self.component.take_replacement()
        }
        fn take_save_status_presentation(&mut self) -> bool {
            self.component.take_save_status_changed()
        }
        fn take_component_close_reports(
            &mut self,
        ) -> Vec<crate::application::ComponentCloseReport> {
            self.component.take_close_reports()
        }
    }

    impl crate::view::capability::ViewState for BoundAppWidget {
        fn view_state(&self) -> Option<crate::view::ViewContribution> {
            self.in_root(|root| crate::view::capability::view_state(root))
                .flatten()
        }
        fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
            self.in_root_mut(|root| crate::view::capability::restore_view_state(root, view));
        }
    }

    #[cfg(feature = "model")]
    impl crate::resource::capability::ResourceScoped for BoundAppWidget {
        fn record_intent(&self) -> Option<crate::resource::DocumentCheckout> {
            self.in_root(|root| crate::resource::capability::record_intent(root))
                .flatten()
        }
        fn set_resource_scope(&mut self, record: &crate::resource::DocumentId) {
            self.in_root_mut(|root| crate::resource::capability::set_resource_scope(root, record));
        }
    }

    impl tilo_ui_base::presence::capability::PublishesPresence for BoundAppWidget {
        fn presence_key(&self) -> Option<PresenceKey> {
            self.in_root(|root| tilo_ui_base::presence::capability::presence_key(root))
                .flatten()
        }
        #[cfg(feature = "model")]
        fn presence_payload(&self) -> Option<(PresenceKey, ::collaboration::PresencePayload)> {
            self.in_root(|root| tilo_ui_base::presence::capability::presence_payload(root))
                .flatten()
        }
    }

    #[cfg(feature = "model")]
    impl tilo_ui_base::widget::permission::PermissionGuarded for BoundAppWidget {
        fn permission_scope(&self) -> Option<tilo_ui_base::permissions::PermissionScope> {
            self.in_root(|root| tilo_ui_base::widget::permission::permission_scope(root))
                .flatten()
        }
    }

    impl crate::search::capability::SearchSource for BoundAppWidget {
        fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
            self.component.search_providers()
        }
        fn search_provider_descriptors(
            &self,
        ) -> Vec<crate::search::capability::SearchProviderDescriptor> {
            self.component.search_provider_descriptors()
        }
        /// The component's entries for the session's search index: what its root publishes, read inside its
        /// binding as its providers are.
        fn search_index_state(&self) -> Option<crate::search::index_publication::IndexState> {
            self.in_root(|root| crate::search::capability::search_index_state(root))
                .flatten()
        }
        fn search_index_entries(&self) -> Vec<crate::search::index_publication::IndexEntry> {
            self.in_root(|root| crate::search::capability::search_index_entries(root))
                .unwrap_or_default()
        }
        fn activate_search_result(
            &mut self,
            action_id: &str,
            doc: &mut tilo_ui_base::widget::Edits<'_>,
            _path: &tilo_ui_base::widget::ModelPath,
        ) {
            self.with_root_edits(doc, |root, edits, resource_root| {
                crate::search::capability::activate_search_result(
                    root,
                    action_id,
                    edits,
                    resource_root,
                );
            });
        }
    }

    impl tilo_ui_base::widget::resources::PublishesResources for BoundAppWidget {
        fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let Some(root) = self.component.root.as_deref() else {
                        return Vec::new();
                    };
                    tilo_ui_base::widget::resources::resources(root, document)
                })
                .unwrap_or_else(|_| Vec::new())
        }
        fn action_directory_resource_page(
            &self,
            doc: &AutoCommit,
            cursor: &[u8],
            max_results: usize,
        ) -> Option<ActionDirectoryResourcePage> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let root = self.component.root.as_deref()?;
                    tilo_ui_base::widget::resources::action_directory_resource_page(
                        root,
                        document,
                        cursor,
                        max_results,
                    )
                })
                .unwrap_or(None)
        }
        fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let root = self.component.root.as_deref()?;
                    tilo_ui_base::widget::resources::action_directory_revision(root, document)
                })
                .unwrap_or(None)
        }
        fn action_directory_invocation_domain(
            &self,
            doc: &AutoCommit,
        ) -> tilo_ui_base::widget::ActionDirectoryInvocationDomain {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    self.component
                        .root
                        .as_deref()
                        .map(|root| {
                            tilo_ui_base::widget::resources::action_directory_invocation_domain(
                                root, document,
                            )
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        }
        fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::ResourceSpec> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let root = self.component.root.as_deref()?;
                    tilo_ui_base::widget::resources::resource(root, document, id)
                })
                .unwrap_or(None)
        }
        /// A presence's tree is built against the component's own record document, like
        /// [`Self::resources`]; the default would fall back to this boundary's own view (the published
        /// schema) on the outer doc.
        fn resources_for(
            &self,
            presence: Option<&tilo_actions::PresenceId>,
            doc: &AutoCommit,
        ) -> Vec<tilo_actions::ResourceSpec> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let Some(root) = self.component.root.as_deref() else {
                        return Vec::new();
                    };
                    tilo_ui_base::widget::resources::resources_for(root, presence, document)
                })
                .unwrap_or_else(|_| Vec::new())
        }
        fn resource_for(
            &self,
            presence: Option<&tilo_actions::PresenceId>,
            doc: &AutoCommit,
            id: &str,
        ) -> Option<tilo_actions::ResourceSpec> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let root = self.component.root.as_deref()?;
                    tilo_ui_base::widget::resources::resource_for(root, presence, document, id)
                })
                .unwrap_or(None)
        }
        fn action_directory_visibility(
            &self,
            doc: &AutoCommit,
            resource_id: &str,
        ) -> Option<tilo_actions::ActionAddress> {
            let binding = self.component.callback_binding();
            binding
                .read_document(doc, |document, _resource_root| {
                    let root = self.component.root.as_deref()?;
                    tilo_ui_base::widget::resources::action_directory_visibility(
                        root,
                        document,
                        resource_id,
                    )
                })
                .unwrap_or(None)
        }
    }

    impl tilo_ui_base::widget::controls::ActionControls for BoundAppWidget {
        fn attach_controls(
            &mut self,
            parent: &tilo_ui_base::ControlDomain,
            attachment: tilo_ui_base::control_domain::DomainAttachment,
        ) {
            let binding = self.component.callback_binding();
            if let Some(domain) = binding.control_domain.upgrade() {
                let _ = domain.attach_to(parent);
                if let Some(root) = self.component.root.as_deref_mut() {
                    tilo_ui_base::widget::controls::attach_controls(root, &domain, attachment);
                }
            }
        }
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return None;
            };
            let root = self.component.root.as_deref_mut()?;
            if let Some(intent) = tilo_ui_base::widget::controls::take_control_invocation(root) {
                let mut queued = binding.queued_controls.borrow_mut();
                if queued.len() >= 64 {
                    queued.pop_front();
                }
                queued.push_back(intent);
            }
            None
        }
        fn on_control_invocation_result(
            &mut self,
            _: &tilo_actions::ActionAddress,
            _: &Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError>,
        ) {
        }
        fn has_pending_control_invocation(&self) -> bool {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return false;
            };
            !binding.queued_controls.borrow().is_empty()
                || self.component.root.as_deref().is_some_and(|root| {
                    tilo_ui_base::widget::controls::has_pending_control_invocation(root)
                })
        }
        fn action_control_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
            self.in_root(|root| tilo_ui_base::widget::controls::action_control_bindings(root))
                .unwrap_or_default()
        }
        fn has_action_control_bindings(&self) -> bool {
            self.in_root(|root| tilo_ui_base::widget::controls::has_action_control_bindings(root))
                .unwrap_or_default()
        }
        fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
            self.in_root(|root| tilo_ui_base::widget::controls::control_action_bindings(root))
                .unwrap_or_default()
        }
        fn refresh_control_action_availability(
            &mut self,
            _allowed: &HashSet<tilo_actions::ActionAddress>,
        ) {
        }
    }

    #[cfg(feature = "model")]
    impl crate::history_mode::capability::DocumentHistory for BoundAppWidget {
        fn history_registration(
            &self,
        ) -> Result<
            Option<crate::history_mode::HistoryRegistration>,
            crate::history_mode::HistoryRegistrationError,
        > {
            self.in_root(|root| {
                Ok(crate::history_mode::capability::history_registration(root)?
                    .filter(crate::history_mode::HistoryRegistration::is_folder_only))
            })
            .unwrap_or(Ok(None))
        }

        fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
            let binding = self.component.callback_binding();
            let _entry = binding
                .enter()
                .map_err(|error| format!("Component is unavailable: {error:?}"))?;
            let root = self
                .component
                .root
                .as_deref()
                .ok_or("Component root is unavailable")?;
            crate::history_mode::capability::history_restore_readiness(root, scope)
        }
    }

    impl tilo_ui_base::widget::undo_access::LocalUndo for BoundAppWidget {
        fn local_undo_status(&self) -> Option<tilo_ui_base::widget::undo_access::LocalUndoStatus> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            tilo_ui_base::widget::undo_access::local_undo_status(self.component.root.as_deref()?)
        }

        fn local_undo_is_deferred(&self) -> bool {
            self.in_root(|root| tilo_ui_base::widget::undo_access::local_undo_is_deferred(root))
                .unwrap_or_default()
        }

        fn handle_local_undo_request(
            &mut self,
            request: tilo_ui_base::widget::undo_access::UndoRequest,
        ) -> bool {
            self.in_root_mut(|root| {
                tilo_ui_base::widget::undo_access::handle_local_undo_request(root, request)
            })
            .unwrap_or_default()
        }
    }

    impl tilo_ui_base::widget::undo_access::UndoControl for BoundAppWidget {}

    impl tilo_ui_base::widget::undo_access::CommittedActions for BoundAppWidget {
        fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
            None
        }
    }

    #[cfg(feature = "model")]
    impl tilo_ui_base::widget::presence_topology::Presences for BoundAppWidget {
        fn join_presence(&mut self, presence: &tilo_actions::PresenceId) -> bool {
            tilo_ui_base::widget::presence_topology::join_presence_in_children(self, presence)
        }
        fn leave_presence(&mut self, presence: &tilo_actions::PresenceId) {
            tilo_ui_base::widget::presence_topology::leave_presence_in_children(self, presence);
        }
        fn set_presence_label(&mut self, presence: &tilo_actions::PresenceId, label: &str) {
            tilo_ui_base::widget::presence_topology::set_presence_label_in_children(
                self, presence, label,
            );
        }
        fn set_remote_only(&mut self, host: bool) {
            tilo_ui_base::widget::presence_topology::set_remote_only_in_children(self, host);
        }
        /// Presence views (agent carets) live against the component's own record document, the one
        /// `sync` hands the app, never the outer App document: resolved against the outer document a Writer
        /// caret finds no block, and no presence is produced at all.
        fn presences(&self, state: &AutoCommit) -> Vec<tilo_ui_base::widget::ViewPresence> {
            let binding = self.component.callback_binding();
            binding
                .read_document(state, |document, _resource_root| {
                    self.component
                        .root
                        .as_deref()
                        .map(|root| {
                            tilo_ui_base::widget::presence_topology::presences(root, document)
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        }
    }
    impl AppWidget for BoundAppWidget {
        fn app_kind(&self) -> AppKind {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return AppKind::Editor;
            };
            self.component
                .root
                .as_deref()
                .map_or(AppKind::Editor, AppWidget::app_kind)
        }
        fn app_title(&self) -> &str {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return "Closed";
            };
            self.component
                .root
                .as_deref()
                .map_or("Closed", AppWidget::app_title)
        }
        fn app_icon(&self) -> IconDescriptor {
            let binding = self.component.callback_binding();
            let Ok(_entry) = binding.enter() else {
                return IconDescriptor::Glyph("");
            };
            self.component
                .root
                .as_deref()
                .map_or(IconDescriptor::Glyph(""), AppWidget::app_icon)
        }
        fn permission_hint(&self) -> Option<AppKind> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            self.component.root.as_deref()?.permission_hint()
        }
        fn device_shell_operation(
            &mut self,
            operation: &crate::floating_surface::DeviceShellOperation,
        ) -> Result<serde_json::Value, String> {
            let binding = self.component.callback_binding();
            let _entry = binding
                .enter()
                .map_err(|_| "Terminal is not available".to_string())?;
            self.component
                .root
                .as_deref_mut()
                .ok_or("Terminal is not available")?
                .device_shell_operation(operation)
        }
        fn request_device_shell(&mut self) -> bool {
            self.in_root_mut(AppWidget::request_device_shell)
                .unwrap_or_default()
        }
        fn take_process_intent(&mut self) -> Option<AppProcessIntent> {
            let binding = self.component.callback_binding();
            let _entry = binding.enter().ok()?;
            if let Some(request) = binding.take_search_request() {
                return Some(AppProcessIntent::OpenSearch(request));
            }
            self.component.root.as_deref_mut()?.take_process_intent()
        }
    }
}
#[cfg(feature = "model")]
pub use component_adapter::BoundAppWidget;

#[cfg(test)]
#[cfg(feature = "model")]
mod native_lifecycle_tests {
    use super::*;
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support as probe;

    tilo_test_env::scenario! {
        /// History status and the lifecycle walk of a bound native owner.
        /// Each phase was a test of its own and builds its own fixture.
        fn the_native_lifecycle_crosses_only_the_live_bound_owner;

        phase free_native_lifecycle_walk_enters_bound_owner_once_and_refuses_closed_owner;
        {
            let host = probe::ProbeHarness::new();
            let component =
                probe::instantiate_probe(&host, 41, LaunchTarget::New, RestoreInput::None).unwrap();
            let binding = component.callback_binding();
            let mut widget = BoundAppWidget::new(component);
            let native_host = tilo_ui_base::widget::NativeViewHost::headless(
                tilo_ui_base::platform::PlatformConventions::default(),
                1.0,
            );

            tilo_ui_base::widget::mount_native_views(&mut widget, &native_host);
            tilo_ui_base::widget::native_view_frame_start(&mut widget);
            tilo_ui_base::widget::unmount_native_views(&mut widget);
            let observations = probe::take_native_lifecycle_observations();
            assert_eq!(
                observations
                    .iter()
                    .map(|(event, _)| *event)
                    .collect::<Vec<_>>(),
                vec!["mount", "frame", "unmount"]
            );
            assert!(
                observations
                    .iter()
                    .all(|(_, lifetime)| lifetime.is_active())
            );

            binding.closing.set(true);
            tilo_ui_base::widget::mount_native_views(&mut widget, &native_host);
            tilo_ui_base::widget::native_view_frame_start(&mut widget);
            tilo_ui_base::widget::unmount_native_views(&mut widget);
            assert!(probe::take_native_lifecycle_observations().is_empty());

            widget.component.close();
            assert!(
                observations
                    .iter()
                    .all(|(_, lifetime)| !lifetime.is_active())
            );
            let teardown = probe::take_native_lifecycle_observations();
            assert_eq!(
                teardown.iter().map(|(event, _)| *event).collect::<Vec<_>>(),
                vec!["unmount"]
            );
            assert!(teardown.iter().all(|(_, lifetime)| !lifetime.is_active()));
        }
    }
}
