//! Shared local-only History chrome for native and headless hosts.
//!
//! This is deliberately not an application-root wrapper. Hosts retain the app
//! root and all its lifecycle hooks, and admit this chrome as an unbound sibling
//! in the same layout arena. `local_resources` — the pane's session, selection
//! and review controls — must never enter a broker or remote application's
//! catalogue. `invoke_local` requires a trusted local UI dispatch path; a
//! matching actor identity is not proof of locality. The broker-visible door
//! onto the same History model is the permission-gated `tilo.history`
//! resource in `app/history_actions.rs`.

use std::collections::HashMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId};
use serde::{Deserialize, Serialize};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_actions::{
    ActionBinding, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::history_source::AuthorDirectory;
use crate::i18n::I18nManager;
use crate::presence::PresenceCtx;
use crate::standalone_history::{
    ConfirmedHistoryRestore, HistoryError, HistoryReadContext, HistoryTarget, RestoreReview,
    StandaloneHistory,
};
use crate::standalone_history_pane::{HistoryPaneIntent, StandaloneHistoryPane};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::layout::CompositionFocus;
use crate::widget::{Button, HistoryEntryId, TaffyTreeExt, Widget};

use crate::search::search_resource;

pub(crate) const HISTORY_OPERATION: &str = "tilo.host.history.view";
pub(crate) const HISTORY_RESOURCE: &str = "tilo.host:history";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HistoryCommand {
    Open,
    /// Open History with one change already selected.
    OpenAt {
        entry: String,
    },
    /// Close whichever History session is showing.
    Hide,
    Select {
        session: uuid::Uuid,
        entries: Vec<String>,
    },
    Compare {
        session: uuid::Uuid,
    },
    Close {
        session: uuid::Uuid,
    },
    ReviewRestore {
        session: uuid::Uuid,
        entry: String,
    },
    Restore {
        session: uuid::Uuid,
    },
    CancelRestore {
        session: uuid::Uuid,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HistoryHostError {
    History(HistoryError),
    InvalidOperation,
    StaleSession,
}

impl From<HistoryError> for HistoryHostError {
    fn from(error: HistoryError) -> Self {
        Self::History(error)
    }
}

/// Layout identities belong to the caller's arena and must be discarded on
/// every structure rebuild. No raw widget references survive such a rebuild.
#[derive(Clone, Copy)]
pub(crate) struct HostHistoryLayout {
    pub root: NodeId,
    pub app: NodeId,
    /// The in-window History button, on hosts with no native chrome.
    pub toolbar: Option<NodeId>,
    pub search: Option<NodeId>,
    /// The strip holding the in-window buttons, when there are any.
    bar: Option<NodeId>,
    pub pane: Option<NodeId>,
    /// The selected version, drawn by the app's own root, tiled where the
    /// live app would be.
    pub preview: Option<NodeId>,
    pub app_visible: bool,
    content: NodeId,
}

pub(crate) struct StandaloneHistoryHost {
    session: Option<StandaloneHistory>,
    session_id: Option<uuid::Uuid>,
    /// In-window History and Search buttons: the fallback for hosts whose
    /// own chrome (a menu bar, the Tilo shell) does not present them.
    toolbar: Option<Button>,
    search: Option<Button>,
    search_enabled: bool,
    pane: StandaloneHistoryPane,
    content_target: Result<HistoryTarget, HistoryError>,
    content_focus: Option<CompositionFocus>,
    restore_focus: Option<CompositionFocus>,
    content_rect: Rect,
    bar_rect: Rect,
    revision: u64,
    review: Option<RestoreReview>,
    confirmed_restore: Option<ConfirmedHistoryRestore>,
    structure_dirty: bool,
    canvas: crate::history_mode::canvas::PreviewCanvas,
    /// Heads of the version whose preview the host still has to build.
    preview_request: Option<Vec<automerge::ChangeHash>>,
    preview_visible: bool,
    preview_root: Option<PreviewRootFactory>,
}

/// Builds a fresh, read-only instance of the app's own root for a preview.
pub(crate) type PreviewRootFactory = std::rc::Rc<dyn Fn() -> Result<Box<dyn Widget>, String>>;

/// The pane message for a refused or completed restore.
fn restore_message(result: &Result<bool, HistoryError>) -> &'static str {
    match result {
        Ok(_) => "standalone-history-restore-done",
        Err(HistoryError::StaleReview | HistoryError::Stale) => "standalone-history-restore-stale",
        Err(HistoryError::PendingEdits) => "standalone-history-restore-pending",
        Err(HistoryError::Denied) => "standalone-history-restore-denied",
        Err(_) => "standalone-history-restore-unavailable",
    }
}

impl StandaloneHistoryHost {
    #[expect(
        clippy::expect_used,
        reason = "fixed unit View operation and its nullary binding serialize infallibly"
    )]
    pub(crate) fn new() -> Self {
        let resource = open_resource();
        let binding = ActionBinding::unit_for(&resource, "open").expect("unit history action");
        Self {
            session: None,
            session_id: None,
            toolbar: Some(
                Button::new("standalone-history-open", "standalone-history-title", None)
                    .on_action(binding)
                    .with_enabled(false),
            ),
            search: None,
            search_enabled: false,
            pane: StandaloneHistoryPane::new(),
            content_target: Err(HistoryError::Unavailable),
            content_focus: None,
            restore_focus: None,
            content_rect: Rect::ZERO,
            bar_rect: Rect::ZERO,
            revision: 0,
            review: None,
            confirmed_restore: None,
            structure_dirty: false,
            canvas: crate::history_mode::canvas::PreviewCanvas::new(),
            preview_request: None,
            preview_visible: false,
            preview_root: None,
        }
    }

    pub(crate) fn set_preview_root(&mut self, factory: PreviewRootFactory) {
        self.preview_root = Some(factory);
    }

    pub(crate) fn preview_root(&self) -> Option<PreviewRootFactory> {
        self.preview_root.clone()
    }

    /// Whether a selected version waits for its preview root. Building the
    /// root belongs to the app host, which owns the wake scope it runs in.
    pub(crate) fn has_preview_request(&self) -> bool {
        self.preview_request.is_some()
    }

    /// Show the requested version drawn by `root`, a fresh read-only instance
    /// of the app's root; without one, the version's document outline.
    pub(crate) fn install_preview(
        &mut self,
        root: Option<Box<dyn Widget>>,
        context: &HistoryReadContext<'_>,
    ) {
        let Some(heads) = self.preview_request.take() else {
            return;
        };
        let doc = crate::history_mode::frozen_root::document_at(context.document, &heads);
        let root = root.unwrap_or_else(|| crate::history_mode::frozen_root::outline_root(&doc));
        self.canvas.install(Ok(Box::new(
            crate::history_mode::frozen_root::FrozenRootPreview::new(
                root,
                doc,
                I18nManager::lookup_plain("standalone-history-preview-resource"),
            ),
        )));
        self.structure_dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    fn clear_preview(&mut self) {
        self.preview_request = None;
        if self.canvas.preview.is_some() {
            self.canvas.clear();
            self.structure_dirty = true;
        }
    }

    pub(crate) fn preview_mut(
        &mut self,
    ) -> Option<&mut crate::history_mode::canvas::PreviewCanvas> {
        self.preview_visible.then_some(&mut self.canvas)
    }

    /// A restore the person confirmed against the still-current frontier.
    /// The host applies it to its live state, then reports the outcome.
    pub(crate) fn take_confirmed_restore(&mut self) -> Option<ConfirmedHistoryRestore> {
        self.confirmed_restore.take()
    }

    pub(crate) fn report_restore(&mut self, result: &Result<bool, HistoryError>) {
        self.pane
            .set_comparison_summary(Some(restore_message(result)));
        self.revision = self.revision.wrapping_add(1);
    }

    /// Whether the presented pane changed shape (a review opened or closed)
    /// since the last call, so the caller must rebuild rather than relayout.
    pub(crate) fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    fn close_review(&mut self) {
        if self.review.take().is_some() || self.pane.review_is_open() {
            self.pane.close_review();
            self.structure_dirty = true;
        }
    }

    fn sync_restore_control(&mut self, context: &HistoryReadContext<'_>) {
        let enabled = self
            .session
            .as_ref()
            .is_some_and(|session| session.selection().len() == 1 && session.can_restore(context));
        self.pane.set_restore_enabled(enabled);
    }

    /// Call while focus still belongs to app content, before dispatching a
    /// pointer/key event into chrome. Never replace this from chrome focus.
    /// Target capture is host-specific; the captured model remains shared.
    pub(crate) fn remember_content_target(
        &mut self,
        target: Result<HistoryTarget, HistoryError>,
        focus: Option<CompositionFocus>,
    ) {
        if self.session.is_none() {
            self.content_target = target;
            self.content_focus = focus;
            let can_open = self.can_open();
            if let Some(toolbar) = &mut self.toolbar {
                toolbar.set_enabled(can_open);
            }
        }
    }

    /// Whether Open would find a document to show History for.
    pub(crate) fn can_open(&self) -> bool {
        self.content_target.is_ok()
    }

    /// Selected entries in the open session, for commands that need two.
    pub(crate) fn selection_len(&self) -> usize {
        self.session
            .as_ref()
            .map_or(0, |session| session.selection().len())
    }

    /// The host's own chrome presents History and Search: drop the in-window
    /// buttons. Their actions stay, reachable from that chrome.
    #[cfg(any(feature = "native", test))]
    pub(crate) fn remove_in_window_controls(&mut self) {
        let had_toolbar = self.toolbar.take().is_some();
        let had_search = self.search.take().is_some();
        self.structure_dirty |= had_toolbar || had_search;
    }

    /// The in-window buttons, for draining and refreshing their invocations.
    pub(crate) fn in_window_controls_mut(&mut self) -> impl Iterator<Item = &mut Button> {
        self.toolbar.iter_mut().chain(self.search.iter_mut())
    }

    /// Presence is a host capability, not an OS check. Hosts without the
    /// existing `ActionPalette` must leave this disabled (not a dead button).
    #[expect(
        clippy::expect_used,
        reason = "fixed nullary Search action exists in the local host resource"
    )]
    pub(crate) fn enable_search(&mut self, platform: crate::platform::PlatformConventions) {
        if !self.search_enabled && self.toolbar.is_some() {
            let binding =
                ActionBinding::unit_for(&search_resource(), "open").expect("unit Search binding");
            let shortcut = crate::platform::HostShortcut::Search
                .shortcut(platform)
                .label(platform);
            self.search = Some(
                Button::new("standalone-search-open", "standalone-search-title", None)
                    .tooltip(format!(
                        "{} ({shortcut})",
                        I18nManager::lookup_plain("standalone-search-title")
                    ))
                    .on_action(binding),
            );
        }
        self.search_enabled = true;
    }
    pub(crate) fn search_mut(&mut self) -> Option<&mut Button> {
        self.search.as_mut()
    }
    pub(crate) fn has_search(&self) -> bool {
        self.search_enabled
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn is_open(&self) -> bool {
        self.session.is_some()
    }
    pub(crate) fn take_restore_focus(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Option<CompositionFocus> {
        let focus = self.restore_focus.take()?;
        StandaloneHistory::open(
            self.content_target.clone().ok()?,
            context,
            AuthorDirectory::new(),
        )
        .ok()?;
        Some(focus)
    }

    /// Rechecks authority even when document heads have not changed. Refused
    /// sessions disappear before any further drawing or resource observation.
    pub(crate) fn refresh(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<bool, HistoryHostError> {
        let Some(session) = self.session.as_mut() else {
            return Ok(false);
        };
        match session.refresh(context) {
            Ok(changed) => {
                if changed {
                    self.pane.set_entries(session.rows());
                    self.pane.set_selected_entries(session.selection());
                    self.pane.set_comparison_summary(None);
                    self.revision = self.revision.wrapping_add(1);
                }
                self.sync_restore_control(context);
                Ok(changed)
            }
            Err(error) => {
                self.content_focus = None;
                self.close();
                Err(error.into())
            }
        }
    }

    /// Only for a known-local host route, never for broker/remote enumeration.
    pub(crate) fn local_resources(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<Vec<Resource>, HistoryHostError> {
        self.refresh(context)?;
        Ok(self.own_resources())
    }

    /// History, Search and the open session, each action available exactly
    /// when invoking it would do something.
    fn own_resources(&self) -> Vec<Resource> {
        let mut resources = vec![history_resource(self.can_open(), self.is_open())];
        if self.has_search() {
            resources.push(search_resource());
        }
        if let Some(session) = self.session_id {
            let mut session = session_resource(session);
            if self.selection_len() < 2
                && let Some(compare) = session
                    .actions
                    .iter_mut()
                    .find(|action| action.id == "compare")
            {
                compare.contract.availability =
                    unavailable("selection_too_small", "host-history-compare-unavailable");
            }
            resources.push(session);
        }
        resources
    }

    /// Execute only after the host establishes trusted local dispatch. This
    /// method does not infer locality from document ownership or actor equality.
    pub(crate) fn invoke_local(
        &mut self,
        encoded: &EncodedAction,
        context: &HistoryReadContext<'_>,
    ) -> Result<(), HistoryHostError> {
        if encoded.type_id != HISTORY_OPERATION {
            return Err(HistoryHostError::InvalidOperation);
        }
        let command: HistoryCommand = serde_json::from_slice(&encoded.payload)
            .map_err(|_| HistoryHostError::InvalidOperation)?;
        self.execute(command, context)
    }

    fn execute(
        &mut self,
        command: HistoryCommand,
        context: &HistoryReadContext<'_>,
    ) -> Result<(), HistoryHostError> {
        if let HistoryCommand::Hide = command {
            if self.session.is_some() {
                self.close();
            }
            return Ok(());
        }
        if let HistoryCommand::OpenAt { entry } = command {
            self.execute(HistoryCommand::Open, context)?;
            let session = self.session_id.ok_or(HistoryHostError::StaleSession)?;
            return self.execute(
                HistoryCommand::Select {
                    session,
                    entries: vec![entry],
                },
                context,
            );
        }
        if let HistoryCommand::Open = command {
            // Repeated Open retains the original captured document and focus.
            if self.session.is_some() {
                self.refresh(context)?;
                return Ok(());
            }
            let target = self.content_target.clone()?;
            let session = StandaloneHistory::open(target, context, AuthorDirectory::new())?;
            self.pane.clear();
            self.pane.set_entries(session.rows());
            self.session = Some(session);
            self.session_id = Some(uuid::Uuid::new_v4());
            self.revision = self.revision.wrapping_add(1);
            return Ok(());
        }
        self.refresh(context)?;
        let expected = self.session_id.ok_or(HistoryHostError::StaleSession)?;
        match command {
            HistoryCommand::Open | HistoryCommand::OpenAt { .. } | HistoryCommand::Hide => {
                return Err(HistoryHostError::InvalidOperation);
            }
            HistoryCommand::Close { session } if session == expected => self.close(),
            HistoryCommand::Select { session, entries } if session == expected => {
                if entries.len() > crate::history_source::DEFAULT_MAX_ENTRIES {
                    return Err(HistoryHostError::InvalidOperation);
                }
                self.pane.set_comparison_summary(None);
                let ids = entries
                    .iter()
                    .map(|entry| decode_entry(entry))
                    .collect::<Result<Vec<_>, _>>()?;
                let model = self
                    .session
                    .as_mut()
                    .ok_or(HistoryHostError::StaleSession)?;
                model.select(context, &ids)?;
                self.pane.set_selected_entries(model.selection());
                if ids.is_empty() {
                    self.pane.set_comparison_summary(None);
                } else {
                    model.compare_selection(context)?;
                    self.update_summary();
                }
                match ids.as_slice() {
                    [id] => {
                        let mut committed = context.document.clone();
                        if committed.pending_ops() > 0 {
                            committed.rollback();
                        }
                        self.preview_request =
                            crate::history_source::heads_for_entry(&mut committed, id);
                    }
                    _ => self.clear_preview(),
                }
            }
            HistoryCommand::Compare { session } if session == expected => {
                self.session
                    .as_mut()
                    .ok_or(HistoryHostError::StaleSession)?
                    .compare_selection(context)?;
                self.update_summary();
            }
            HistoryCommand::ReviewRestore { session, entry } if session == expected => {
                let id = decode_entry(&entry)?;
                let model = self
                    .session
                    .as_mut()
                    .ok_or(HistoryHostError::StaleSession)?;
                match model.review_restore(context, id) {
                    Ok(review) => {
                        self.pane.open_review(&review.label);
                        self.review = Some(review);
                        self.structure_dirty = true;
                    }
                    Err(error) => {
                        self.report_restore(&Err(error.clone()));
                        return Err(error.into());
                    }
                }
            }
            HistoryCommand::Restore { session } if session == expected => {
                let review = self.review.take().ok_or(HistoryHostError::StaleSession)?;
                self.close_review();
                let model = self
                    .session
                    .as_ref()
                    .ok_or(HistoryHostError::StaleSession)?;
                if let Err(error) = model.validate_restore(context, &review) {
                    self.report_restore(&Err(error.clone()));
                    return Err(error.into());
                }
                self.confirmed_restore = Some(ConfirmedHistoryRestore {
                    review,
                    target: model.target().clone(),
                });
            }
            HistoryCommand::CancelRestore { session } if session == expected => {
                self.close_review();
            }
            _ => return Err(HistoryHostError::StaleSession),
        }
        self.sync_restore_control(context);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    fn update_summary(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let details = session
            .selection()
            .iter()
            .filter_map(|id| session.rows().iter().find(|entry| entry.id == *id))
            .map(|entry| format!("{}: {}", entry.author.name, entry.summary))
            .collect::<Vec<_>>()
            .join("\n");
        // These are selected-change details. Exact comparison snapshots remain
        // available through session(); a resource renderer owns diff display.
        let summary = format!(
            "{}\n{details}",
            I18nManager::lookup_plain("standalone-history-selected-details")
        );
        self.pane.set_comparison_summary(Some(&summary));
    }

    fn close(&mut self) {
        self.clear_preview();
        self.close_review();
        self.confirmed_restore = None;
        self.session = None;
        self.session_id = None;
        self.pane.clear();
        self.restore_focus = self.content_focus.take();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Drain after canonical widget operation/semantic dispatch. Both pointer
    /// range selection and AT row activation use the same validated commands.
    pub(crate) fn process_pane_intents(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<bool, HistoryHostError> {
        self.refresh(context)?;
        let mut changed = false;
        while let Some(intent) = self.pane.take_intent() {
            let session = self.session_id.ok_or(HistoryHostError::StaleSession)?;
            let command = match intent {
                HistoryPaneIntent::Close => HistoryCommand::Close { session },
                HistoryPaneIntent::ClearSelection => HistoryCommand::Select {
                    session,
                    entries: Vec::new(),
                },
                HistoryPaneIntent::Select(entry) => HistoryCommand::Select {
                    session,
                    entries: vec![encode_entry(entry)],
                },
                HistoryPaneIntent::Compare(entries) => HistoryCommand::Select {
                    session,
                    entries: entries.into_iter().map(encode_entry).collect(),
                },
                HistoryPaneIntent::ReviewRestore => {
                    let Some([entry]) = self.session.as_ref().map(StandaloneHistory::selection)
                    else {
                        continue;
                    };
                    HistoryCommand::ReviewRestore {
                        session,
                        entry: encode_entry(*entry),
                    }
                }
                HistoryPaneIntent::ConfirmRestore => HistoryCommand::Restore { session },
                HistoryPaneIntent::CancelRestore => HistoryCommand::CancelRestore { session },
            };
            self.execute(command, context)?;
            changed = true;
        }
        Ok(changed)
    }

    /// Refresh immediately before using these roots for semantic/AT traversal.
    /// They are unbound siblings; never inherit the app root's callback binding.
    pub(crate) fn presentation_roots(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<Vec<&dyn Widget>, HistoryHostError> {
        self.refresh(context)?;
        let mut roots: Vec<&dyn Widget> = Vec::new();
        roots.extend(self.toolbar.as_ref().map(|button| button as &dyn Widget));
        roots.extend(self.search.as_ref().map(|button| button as &dyn Widget));
        if self.is_open() {
            roots.push(&self.pane);
        }
        if self.preview_visible {
            roots.push(&self.canvas);
        }
        Ok(roots)
    }

    /// Borrow a traversal-only view of the presentation forest. The callback
    /// receives only an immutable widget reference, so this projection cannot
    /// replace the app root or run its lifecycle. Node maps contain pointers to
    /// the actual app/chrome widgets, NEVER to this stack-local projection.
    pub(crate) fn with_presentation<R>(
        &mut self,
        context: &HistoryReadContext<'_>,
        app: &dyn Widget,
        layout: HostHistoryLayout,
        rect: Rect,
        visit: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, HistoryHostError> {
        self.refresh(context)?;
        self.with_validated_presentation(context, app, layout, rect, visit)
    }

    /// Inspect only the already rendered frame, after rechecking its captured
    /// authority. Mutable frame preparation refreshes the actual model/rows.
    pub(crate) fn with_validated_presentation<R>(
        &self,
        context: &HistoryReadContext<'_>,
        app: &dyn Widget,
        layout: HostHistoryLayout,
        rect: Rect,
        visit: impl FnOnce(&dyn Widget) -> R,
    ) -> Result<R, HistoryHostError> {
        self.validate_presentation(context, layout)?;
        let mut content_children: Vec<&dyn Widget> = Vec::new();
        if layout.app_visible {
            content_children.push(app);
        }
        if layout.preview.is_some() {
            content_children.push(&self.canvas);
        }
        if self.is_open() {
            content_children.push(&self.pane);
        }
        let content = PresentationRoots {
            children: content_children,
            layout_root: layout.content,
            rect: self.content_rect,
            id: "standalone-content",
        };
        let buttons: Vec<&dyn Widget> = self
            .toolbar
            .iter()
            .chain(self.search.iter())
            .map(|button| button as &dyn Widget)
            .collect();
        let services = layout.bar.map(|bar| PresentationRoots {
            rect: self.bar_rect,
            children: buttons,
            layout_root: bar,
            id: "standalone-services",
        });
        let mut children: Vec<&dyn Widget> = Vec::new();
        if let Some(services) = &services {
            children.push(services);
        }
        children.push(&content);
        let roots = PresentationRoots {
            children,
            layout_root: layout.root,
            rect,
            id: "standalone-presentation",
        };
        Ok(visit(&roots))
    }
    pub(crate) fn validate_presentation(
        &self,
        context: &HistoryReadContext<'_>,
        layout: HostHistoryLayout,
    ) -> Result<(), HistoryHostError> {
        if layout.pane.is_some() != self.is_open() {
            return Err(HistoryHostError::StaleSession);
        }
        if let Some(session) = &self.session {
            session.validate(context)?;
        }
        Ok(())
    }

    pub(crate) fn validated_local_resources(
        &self,
        context: &HistoryReadContext<'_>,
        layout: HostHistoryLayout,
    ) -> Result<Vec<Resource>, HistoryHostError> {
        self.validate_presentation(context, layout)?;
        let mut resources = self.own_resources();
        if self.is_open() {
            crate::widget::collect_widget_resources(&self.pane, context.document, &mut resources);
        }
        Ok(resources)
    }

    pub(crate) fn toolbar_mut(&mut self) -> Option<&mut Button> {
        self.toolbar.as_mut()
    }
    pub(crate) fn pane_mut(&mut self) -> Option<&mut StandaloneHistoryPane> {
        self.is_open().then_some(&mut self.pane)
    }

    pub(crate) fn session(
        &mut self,
        context: &HistoryReadContext<'_>,
    ) -> Result<Option<&StandaloneHistory>, HistoryHostError> {
        self.refresh(context)?;
        Ok(self.session.as_ref())
    }

    /// App construction and lifecycle stay with the caller. Build chrome only,
    /// then compose its nodes and the already-built app node in the same arena.
    pub(crate) fn build_layout(
        &mut self,
        app: NodeId,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        viewport_width: f64,
    ) -> HostHistoryLayout {
        let toolbar = self
            .toolbar
            .as_mut()
            .map(|button| button.build_layout(taffy, theme, nodes));
        let search = self
            .search
            .as_mut()
            .map(|button| button.build_layout(taffy, theme, nodes));
        let buttons: Vec<NodeId> = toolbar.into_iter().chain(search).collect();
        let bar = (!buttons.is_empty()).then(|| {
            taffy.parent_node(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    padding: crate::chrome::bar_padding(theme),
                    gap: Size {
                        width: taffy::LengthPercentage::length(theme.gap_standard),
                        height: taffy::LengthPercentage::length(0.0),
                    },
                    align_items: Some(taffy::AlignItems::Center),
                    ..Default::default()
                },
                &buttons,
            )
        });
        let pane = self
            .is_open()
            .then(|| self.pane.build_layout(taffy, theme, nodes));
        let wide = viewport_width >= 900.0;
        // A selected version takes the live app's place beside the pane:
        // tiled, never floating over the document it replaces.
        self.preview_visible = pane.is_some() && wide && self.canvas.preview.is_some();
        let preview = self
            .preview_visible
            .then(|| self.canvas.build_layout(taffy, theme, nodes));
        let app_visible = (pane.is_none() || wide) && preview.is_none();
        if let Ok(style) = taffy.style(app) {
            let mut style = style.clone();
            if !app_visible {
                style.display = taffy::Display::None;
            }
            if pane.is_some() && app_visible {
                style.size.width = Dimension::auto();
                style.min_size.width = Dimension::length(0.0);
                style.flex_grow = 1.0;
                style.flex_basis = Dimension::length(0.0);
            }
            let _ = taffy.set_style(app, style);
        }
        if let Some(pane) = pane
            && let Ok(style) = taffy.style(pane)
        {
            let mut style = style.clone();
            style.size.width = if app_visible || preview.is_some() {
                Dimension::length(360.0)
            } else {
                Dimension::percent(1.0)
            };
            style.flex_shrink = 0.0;
            let _ = taffy.set_style(pane, style);
        }
        if let Some(preview) = preview
            && let Ok(style) = taffy.style(preview)
        {
            let mut style = style.clone();
            style.size.width = Dimension::auto();
            style.flex_basis = Dimension::length(0.0);
            let _ = taffy.set_style(preview, style);
        }
        let mut content_children = if app_visible { vec![app] } else { Vec::new() };
        content_children.extend(preview);
        content_children.extend(pane);
        let content = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                flex_grow: 1.0,
                flex_basis: Dimension::length(0.0),
                gap: Size {
                    width: taffy::LengthPercentage::length(theme.gap_standard),
                    height: taffy::LengthPercentage::length(0.0),
                },
                ..Default::default()
            },
            &content_children,
        );
        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_direction: taffy::FlexDirection::Column,
                align_items: Some(taffy::AlignItems::FlexStart),
                ..Default::default()
            },
            &bar.into_iter().chain([content]).collect::<Vec<_>>(),
        );
        HostHistoryLayout {
            root,
            app,
            toolbar,
            search,
            bar,
            pane,
            preview,
            app_visible,
            content,
        }
    }

    /// Apply the caller-owned app with the returned origin and `layout.app`.
    /// This method only applies chrome, never the app root.
    pub(crate) fn apply_layout(
        &mut self,
        layout: HostHistoryLayout,
        taffy: &TaffyTree<()>,
        origin: Point,
    ) -> Point {
        let outer = taffy.layout(layout.root).copied().unwrap_or_default();
        let origin = origin + (f64::from(outer.location.x), f64::from(outer.location.y));
        let bar = layout
            .bar
            .and_then(|node| taffy.layout(node).ok())
            .copied()
            .unwrap_or_default();
        let toolbar_origin = origin + (f64::from(bar.location.x), f64::from(bar.location.y));
        self.bar_rect = Rect::from_origin_size(
            toolbar_origin,
            (f64::from(bar.size.width), f64::from(bar.size.height)),
        );
        if let (Some(toolbar), Some(node)) = (&mut self.toolbar, layout.toolbar) {
            toolbar.apply_layout(taffy, node, toolbar_origin);
        }
        if let (Some(search), Some(node)) = (&mut self.search, layout.search) {
            search.apply_layout(taffy, node, toolbar_origin);
        }
        let content = taffy.layout(layout.content).copied().unwrap_or_default();
        let content_origin =
            origin + (f64::from(content.location.x), f64::from(content.location.y));
        self.content_rect = Rect::from_origin_size(
            content_origin,
            (
                f64::from(content.size.width),
                f64::from(content.size.height),
            ),
        );
        if let Some(pane) = layout.pane {
            self.pane.apply_layout(taffy, pane, content_origin);
        }
        if let Some(preview) = layout.preview {
            self.canvas.apply_layout(taffy, preview, content_origin);
        }
        content_origin
    }

    pub(crate) fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        for button in self.in_window_controls_mut() {
            button.sync(doc, path);
        }
        if self.is_open() {
            self.pane.sync(doc, path);
        }
    }

    pub(crate) fn draw(
        &mut self,
        context: &HistoryReadContext<'_>,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Result<(), HistoryHostError> {
        self.refresh(context)?;
        if self.toolbar.is_some() {
            crate::chrome::paint_bar(scene, transform, theme, self.bar_rect);
        }
        for button in self.in_window_controls_mut() {
            button.draw(scene, transform, text, theme, presence);
        }
        if self.is_open() {
            self.pane.draw(scene, transform, text, theme, presence);
        }
        if self.preview_visible {
            self.canvas.draw(scene, transform, text, theme, presence);
        }
        Ok(())
    }
}

/// Private so callers can obtain only the immutable traversal projection from
/// `with_presentation`. Its required mutable Widget methods are unreachable
/// through that API; no operational app capability is delegated or replaced.
struct PresentationRoots<'a> {
    children: Vec<&'a dyn Widget>,
    layout_root: NodeId,
    rect: Rect,
    id: &'static str,
}

impl Widget for PresentationRoots<'_> {
    fn build_layout(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        _nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_root
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.children.clone()
    }
    fn for_each_child(&self, visit: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.children {
            visit(*child);
        }
    }
    fn name(&self) -> &'static str {
        self.id
    }
    fn test_id(&self) -> Option<&str> {
        Some(self.id)
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[expect(
    clippy::expect_used,
    reason = "fixed View command payloads contain only serializable Rust values"
)]
fn operation(id: &str, label: &str, command: &HistoryCommand) -> ActionSpec {
    ActionSpec::operation(
        id,
        I18nManager::lookup_plain(label),
        OperationKind::View,
        HISTORY_OPERATION,
        command,
    )
    .expect("history command serializes")
}
fn unavailable(code: &str, reason: &str) -> tilo_actions::Availability {
    tilo_actions::Availability::Unavailable {
        code: code.into(),
        reason: I18nManager::lookup_plain(reason),
    }
}

fn open_resource() -> Resource {
    Resource::new(
        HISTORY_RESOURCE,
        "tilo.host.history",
        I18nManager::lookup_plain("standalone-history-title"),
    )
    .with_action(operation(
        "open",
        "standalone-history-title",
        &HistoryCommand::Open,
    ))
    .with_action(open_at_operation())
}

/// The stable History door, whichever History serves it: Open while there
/// is a document to show, and Hide while History is showing.
pub(crate) fn history_resource(can_open: bool, open: bool) -> Resource {
    let mut resource = open_resource();
    if !can_open {
        for action in &mut resource.actions {
            action.contract.availability =
                unavailable("history_unavailable", "host-history-unavailable");
        }
    }
    if open {
        resource = resource.with_action(operation(
            "hide",
            "host-history-hide",
            &HistoryCommand::Hide,
        ));
    }
    resource
}

/// Open at one change: what a recent change in a History menu does. Rows
/// are the palette's own History view, so it is offered to callers only.
#[expect(
    clippy::expect_used,
    reason = "typed History command payload contains only a validated JSON string"
)]
fn open_at_operation() -> ActionSpec {
    ActionSpec::typed_operation(
        "open_at",
        I18nManager::lookup_plain("standalone-history-title"),
        I18nManager::lookup_plain("standalone-history-select-hint"),
        OperationKind::View,
        HISTORY_OPERATION,
        vec![
            ActionParam::required_owned(
                "entry",
                ParamType::Text,
                I18nManager::lookup_plain("standalone-history-entry-ids"),
            )
            .with_schema(serde_json::json!({"pattern": "^[0-9a-f]{32}$"})),
        ],
        |args| {
            let entry = args.text("entry").unwrap_or_default().to_owned();
            EncodedAction::encode_operation(HISTORY_OPERATION, &HistoryCommand::OpenAt { entry })
                .expect("validated history entry serializes")
        },
    )
    .programmatic()
}
#[expect(
    clippy::expect_used,
    reason = "typed History command payload contains only validated JSON strings and a session UUID"
)]
fn session_resource(session: uuid::Uuid) -> Resource {
    let select = ActionSpec::typed_operation("select", I18nManager::lookup_plain("standalone-history-select"), I18nManager::lookup_plain("standalone-history-select-hint"), OperationKind::View, HISTORY_OPERATION,
        vec![ActionParam::required_owned("entries", ParamType::Array, I18nManager::lookup_plain("standalone-history-entry-ids")).with_schema(serde_json::json!({"maxItems": 200, "items": {"type": "string", "pattern": "^[0-9a-f]{32}$"}}))],
        move |args| {
            let entries = args.array("entries").unwrap_or_default().iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect();
            EncodedAction::encode_operation(HISTORY_OPERATION, &HistoryCommand::Select { session, entries }).expect("validated history selection serializes")
        });
    Resource::new(
        format!("tilo.host.history:{session}"),
        "tilo.host.history.session",
        I18nManager::lookup_plain("standalone-history-title"),
    )
    .with_action(select)
    .with_action(operation(
        "compare",
        "standalone-history-compare",
        &HistoryCommand::Compare { session },
    ))
    .with_action(operation(
        "close",
        "standalone-history-close",
        &HistoryCommand::Close { session },
    ))
}
pub(crate) fn encode_entry(id: HistoryEntryId) -> String {
    crate::widget::history_group::hex16(&id)
}
pub(crate) fn decode_entry(value: &str) -> Result<HistoryEntryId, HistoryHostError> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(HistoryHostError::InvalidOperation);
    }
    let mut bytes = [0; 16];
    for (slot, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let text = std::str::from_utf8(pair).map_err(|_| HistoryHostError::InvalidOperation)?;
        *slot = u8::from_str_radix(text, 16).map_err(|_| HistoryHostError::InvalidOperation)?;
    }
    Ok(HistoryEntryId(bytes))
}

#[cfg(test)]
mod tests {
    #[test]
    fn standalone_history_host_search_selection_and_session_follow_host_conventions() {
        // Case `search_is_opt_in_and_uses_host_shortcut_conventions`.
        {
            use crate::platform::{Platform, PlatformConventions, Shortcut};
            use crate::widget::Widget;
            let mut host = super::StandaloneHistoryHost::new();
            assert!(
                !host.has_search(),
                "headless/history-only hosts expose no dead Search control"
            );
            assert!(host.search_mut().is_none());
            for platform in [Platform::Windows, Platform::MacOs] {
                let conventions = PlatformConventions::new(platform);
                let mut host = super::StandaloneHistoryHost::new();
                host.enable_search(conventions);
                let button = host.search_mut().unwrap();
                let shortcut = Shortcut::command("K").label(conventions);
                assert!(Widget::tooltip(button).unwrap().contains(&shortcut));
                assert_eq!(
                    button.accessibility().unwrap().name.as_deref(),
                    Some("Search")
                );
            }
        }

        // Case `canonical_three_change_selection_preserves_typing_and_rejects_old_session`.
        {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "value", "before").unwrap();
            doc.commit();
            let mut ids = Vec::new();
            for value in ["one", "two", "three"] {
                doc.put(ROOT, "value", value).unwrap();
                ids.push(encode_entry(crate::history_source::entry_id_from_hash(
                    &doc.commit().unwrap().to_string(),
                )));
            }
            doc.put(ROOT, "value", "typing").unwrap();
            let pending = doc.pending_ops();
            let identity = uuid::Uuid::new_v4();
            let permissions = PermissionContext::permissive();
            let context = HistoryReadContext {
                document: &doc,
                active_record: None,
                document_identity: identity,
                permissions: &permissions,
            };
            let mut host = StandaloneHistoryHost::new();
            remember(&mut host, identity);
            invoke(&mut host, &HistoryCommand::Open, &context).unwrap();
            let session = host.session_id.unwrap();
            let select = HistoryCommand::Select {
                session,
                entries: ids,
            };
            invoke(&mut host, &select, &context).unwrap();
            let comparison = host.session.as_ref().unwrap().comparison().unwrap();
            assert_eq!(comparison.entries.len(), 3);
            let before = AutoCommit::load(&comparison.before_document).unwrap();
            let after = AutoCommit::load(&comparison.after_document).unwrap();
            assert_eq!(
                before.get(ROOT, "value").unwrap().unwrap().0.as_str(),
                Some("before")
            );
            assert_eq!(
                after.get(ROOT, "value").unwrap().unwrap().0.as_str(),
                Some("three")
            );
            assert_eq!(doc.pending_ops(), pending);
            invoke(&mut host, &HistoryCommand::Close { session }, &context).unwrap();
            invoke(&mut host, &HistoryCommand::Open, &context).unwrap();
            assert_eq!(
                invoke(&mut host, &select, &context),
                Err(HistoryHostError::StaleSession)
            );
            assert!(host.session.as_ref().unwrap().selection().is_empty());
        }

        // Case `replacing_document_removes_private_presentation_and_session_actions`.
        {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "private", "saved").unwrap();
            doc.commit();
            let permissions = PermissionContext::permissive();
            let identity = uuid::Uuid::new_v4();
            let mut context = HistoryReadContext {
                document: &doc,
                active_record: None,
                document_identity: identity,
                permissions: &permissions,
            };
            let mut host = StandaloneHistoryHost::new();
            remember(&mut host, identity);
            invoke(&mut host, &HistoryCommand::Open, &context).unwrap();
            assert_eq!(host.presentation_roots(&context).unwrap().len(), 2);
            let resources = host.local_resources(&context).unwrap();
            assert_eq!(resources.len(), 2);
            assert!(
                resources
                    .iter()
                    .flat_map(|resource| &resource.actions)
                    .all(|action| action.kind == OperationKind::View)
            );
            context.document_identity = uuid::Uuid::new_v4();
            assert_eq!(
                host.local_resources(&context).err(),
                Some(HistoryHostError::History(HistoryError::Stale))
            );
            assert!(!host.is_open());
            assert!(host.pane_mut().is_none());
            assert_eq!(host.presentation_roots(&context).unwrap().len(), 1);
            assert_eq!(host.local_resources(&context).unwrap().len(), 1);
        }

        // Case `shared_arena_composition_preserves_app_grid_and_hides_content_when_narrow`.
        {
            let doc = AutoCommit::new();
            let permissions = PermissionContext::permissive();
            let identity = uuid::Uuid::new_v4();
            let context = HistoryReadContext {
                document: &doc,
                active_record: None,
                document_identity: identity,
                permissions: &permissions,
            };
            let mut host = StandaloneHistoryHost::new();
            remember(&mut host, identity);
            invoke(&mut host, &HistoryCommand::Open, &context).unwrap();
            for search_enabled in [false, true] {
                if search_enabled {
                    host.enable_search(crate::platform::PlatformConventions::new(
                        crate::platform::Platform::Windows,
                    ));
                }
                for width in [1100.0, 500.0] {
                    let mut app = Text::new("app content").with_id("private-app-content");
                    let mut tree = TaffyTree::new();
                    let mut nodes = HashMap::new();
                    let theme = Theme::dark();
                    let app_node = app.build_layout(&mut tree, &theme, &mut nodes);
                    let mut style = tree.style(app_node).unwrap().clone();
                    style.display = taffy::Display::Grid;
                    tree.set_style(app_node, style).unwrap();
                    let layout = host.build_layout(app_node, &mut tree, &theme, &mut nodes, width);
                    assert_eq!(layout.app_visible, width >= 900.0);
                    assert_eq!(
                        tree.style(app_node).unwrap().display,
                        if layout.app_visible {
                            taffy::Display::Grid
                        } else {
                            taffy::Display::None
                        }
                    );
                    assert!(nodes.contains_key(&layout.app));
                    let toolbar = layout.toolbar.unwrap();
                    assert!(nodes.contains_key(&toolbar));
                    assert!(nodes.contains_key(&layout.pane.unwrap()));
                    assert_ne!(layout.app, toolbar);
                    assert_ne!(layout.app, layout.pane.unwrap());
                    assert_eq!(
                        tree.children(layout.root).unwrap(),
                        vec![layout.bar.unwrap(), layout.content]
                    );
                    host.with_presentation(&context, &app, layout, Rect::ZERO, |presentation| {
                        fn check_pairing(
                            widget: &dyn Widget,
                            node: NodeId,
                            tree: &TaffyTree<()>,
                            nodes: &HashMap<NodeId, *mut dyn Widget>,
                            visited: &mut Vec<String>,
                        ) {
                            if let Some(id) = widget.test_id() {
                                visited.push(id.to_owned());
                            }
                            if let Some(pointer) = nodes.get(&node) {
                                assert_eq!(
                                    std::ptr::from_ref(widget).cast::<()>(),
                                    pointer.cast_const().cast::<()>(),
                                    "node must refer to actual retained widget"
                                );
                            } else {
                                assert!(
                                    matches!(
                                        widget.test_id(),
                                        Some(
                                            "standalone-presentation"
                                                | "standalone-content"
                                                | "standalone-services"
                                        )
                                    ),
                                    "only ephemeral traversal containers lack node-map pointers"
                                );
                            }
                            let children = widget.children();
                            let child_nodes = tree.children(node).unwrap();
                            assert_eq!(
                                children.len(),
                                child_nodes.len(),
                                "widget/Taffy child topology must match"
                            );
                            for (child, node) in children.into_iter().zip(child_nodes) {
                                check_pairing(child, node, tree, nodes, visited);
                            }
                        }
                        let mut visited = Vec::new();
                        check_pairing(presentation, layout.root, &tree, &nodes, &mut visited);
                        assert_eq!(
                            visited.iter().any(|id| id == "private-app-content"),
                            layout.app_visible
                        );
                        assert!(visited.iter().any(|id| id == "standalone-history"));
                        assert_eq!(
                            visited
                                .iter()
                                .filter(|id| *id == "standalone-search-open")
                                .count(),
                            usize::from(search_enabled)
                        );
                        assert_eq!(layout.search.is_some(), search_enabled);
                        assert_eq!(
                            tree.children(layout.bar.unwrap()).unwrap(),
                            std::iter::once(toolbar)
                                .chain(layout.search)
                                .collect::<Vec<_>>(),
                            "the bar holds exactly the in-window buttons"
                        );
                        if let Some(search) = layout.search {
                            assert!(nodes.contains_key(&search));
                            assert_eq!(
                                tree.children(layout.bar.unwrap()).unwrap(),
                                vec![toolbar, search]
                            );
                        }
                        assert!(presentation.callback_binding().is_none());
                        assert!(!nodes.contains_key(&layout.root));
                        assert!(!nodes.contains_key(&layout.content));
                    })
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn host_owned_chrome_removes_in_window_buttons_but_keeps_their_actions() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "value", "saved").unwrap();
        let entry = encode_entry(crate::history_source::entry_id_from_hash(
            &doc.commit().unwrap().to_string(),
        ));
        let permissions = PermissionContext::permissive();
        let identity = uuid::Uuid::new_v4();
        let context = HistoryReadContext {
            document: &doc,
            active_record: None,
            document_identity: identity,
            permissions: &permissions,
        };
        let mut host = StandaloneHistoryHost::new();
        remember(&mut host, identity);
        host.enable_search(crate::platform::PlatformConventions::new(
            crate::platform::Platform::MacOs,
        ));
        host.remove_in_window_controls();
        assert!(host.take_structure_dirty());
        assert!(host.presentation_roots(&context).unwrap().is_empty());
        let ids: Vec<_> = host
            .local_resources(&context)
            .unwrap()
            .into_iter()
            .map(|resource| resource.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, [HISTORY_RESOURCE, crate::search::SEARCH_RESOURCE]);

        let mut app = Text::new("app");
        let mut tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let app_node = app.build_layout(&mut tree, &Theme::light(), &mut nodes);
        let layout = host.build_layout(app_node, &mut tree, &Theme::light(), &mut nodes, 1100.0);
        assert!(layout.toolbar.is_none() && layout.search.is_none());
        assert_eq!(tree.children(layout.root).unwrap(), vec![layout.content]);

        // What a recent change in a History menu does: open at that change.
        invoke(
            &mut host,
            &HistoryCommand::OpenAt {
                entry: entry.clone(),
            },
            &context,
        )
        .unwrap();
        assert!(host.is_open());
        assert_eq!(
            host.session.as_ref().unwrap().selection(),
            [decode_entry(&entry).unwrap()]
        );
    }

    use super::*;
    use crate::permissions::PermissionContext;
    use crate::widget::Text;
    use automerge::{ROOT, ReadDoc, transaction::Transactable};

    fn invoke(
        host: &mut StandaloneHistoryHost,
        command: &HistoryCommand,
        context: &HistoryReadContext<'_>,
    ) -> Result<(), HistoryHostError> {
        let encoded = EncodedAction::encode_operation(HISTORY_OPERATION, command).unwrap();
        host.invoke_local(&encoded, context)
    }

    fn remember(host: &mut StandaloneHistoryHost, identity: uuid::Uuid) {
        host.remember_content_target(
            Ok(HistoryTarget::Legacy {
                app_id: "notes".into(),
                record: None,
                document_identity: identity,
            }),
            None,
        );
    }
}
