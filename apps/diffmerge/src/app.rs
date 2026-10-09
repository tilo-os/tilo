//! Root widget — orchestrates the diff and merge modes, the top toolbar
//! (mode switch, change navigation, open), and the host-portal file open.
//!
//! Data flow:
//! * The document holds what is open — the comparison, the merge and its
//!   merged result — and the app is [presented](DiffMergeApp::present_state)
//!   it; the two views ([`DiffView`], [`MergeView`]) draw from that.
//! * The toolbar's buttons are drawn directly and hit-tested in `handle_event`.
//! * Opening files is the [`DiffmergeEffect::OpenFiles`] effect, whatever
//!   asks for it: it goes through the **host** file chooser (the XDG Desktop
//!   Portal, [`ui::portal`]) — the app never draws its own picker (HIG §5) —
//!   or reads the two paths it was given, and opens what it read as a
//!   comparison through an action.

use std::collections::HashMap;
use std::path::Path;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, EmptyState, EmptyStateContent, ModelPath};

use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::framework::{Dispatcher, EffectAnswer, EffectCx, EffectReply, Effected};
use ui::permissions::PermissionContext;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::actions::{DiffmergeAction, DiffmergeEffect};
use crate::chrome::{Chrome, Command, Header};
use crate::diff_view::DiffView;
use crate::doc::{Comparison, MergeInputs, MergeState};
use crate::i18n::{count, t, t_args};
use crate::merge_view::MergeView;
use crate::resource::Diffmerge;
use ui::chrome::status_bar;
use ui::icons::Icon;
use ui::paint::PaintCtx;

/// The error the `error` screenshot scenario shows.
const SHOT_LOAD_ERROR: &str = "Couldn’t read the selected files. Check permissions and try again.";

fn diff_open_request() -> PickRequest {
    PickRequest::open("Open a file to compare", "Open")
        .thread_name("diffmerge-portal-open")
        .filter(
            FileFilter::new("Text & source")
                .mimetype("text/plain")
                .glob("*.txt")
                .glob("*.md")
                .glob("*.rs")
                .glob("*.toml")
                .glob("*.json")
                .glob("*.yaml")
                .glob("*.yml")
                .glob("*.c")
                .glob("*.h")
                .glob("*.py")
                .glob("*.js")
                .glob("*.ts"),
        )
        .all_files()
}

/// Whether chosen files have native paths here; without them (a browser
/// page) the chooser hands over their contents.
fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Diff,
    Merge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    ModeDiff,
    ModeMerge,
    PrevChange,
    NextChange,
    Open,
    LoadSample,
}

impl From<Command> for Action {
    fn from(command: Command) -> Self {
        match command {
            Command::Mode(Mode::Diff) => Self::ModeDiff,
            Command::Mode(Mode::Merge) => Self::ModeMerge,
            Command::Previous => Self::PrevChange,
            Command::Next => Self::NextChange,
            Command::Open => Self::Open,
            Command::LoadSample => Self::LoadSample,
        }
    }
}

/// Pending paint and layout work for the retained views.
struct ViewInvalidation {
    paint: bool,
    structure: bool,
}

pub struct DiffMergeApp {
    mode: Mode,
    diff: DiffView,
    merge: MergeView,
    rect: Rect,
    diff_node: Option<NodeId>,
    merge_node: Option<NodeId>,
    chrome: Option<Chrome>,
    status_h: f64,
    compact: bool,
    /// An in-flight XDG-portal file-open. The portal round-trip runs on a
    /// worker thread and hands the chosen path back over this channel,
    /// drained once per frame in [`Widget::sync`]. `None` when no picker is
    /// in flight — the app never draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,
    /// A chooser delivering contents, where files have no path (the web).
    portal_contents_rx: Option<ContentsChannel>,
    /// Files chosen but not yet paired (first of a two-file diff selection).
    pending_left: Option<(String, String)>,
    /// The caller of the open-files effect a chooser is answering.
    open_reply: Option<EffectReply>,
    /// A human-readable reason the last open failed, surfaced in the status
    /// bubble. `None` in the normal (success / cancel) case.
    load_error: Option<String>,
    /// Paint changes and one-shot layout rebuilds when the active view changes.
    invalidation: ViewInvalidation,
    /// Where edits and effects go; `None` until the framework attaches the
    /// surface, when every edit is dropped.
    dispatcher: Option<Dispatcher<Diffmerge>>,
    /// What was open when the app was last presented the document, so a
    /// newly opened comparison or merge is shown in its mode.
    presented: Option<(Option<Comparison>, Option<MergeInputs>)>,
    /// Capture requests a visible comparison status.
    shots_status: bool,
}

impl DiffMergeApp {
    fn draw_error(ctx: &mut PaintCtx<'_>, rect: Rect, reason: &str) {
        let title = t("error-title");
        let content = EmptyStateContent {
            icon: Some(Icon::TriangleAlert),
            message: Some(reason),
            error: true,
            ..EmptyStateContent::new(&title)
        };
        EmptyState::paint(ctx, rect, &content, Point::new(-1.0, -1.0));
    }

    fn status_readouts(&self) -> (String, String) {
        if self.load_error.is_some() {
            return (String::new(), String::new());
        }
        match self.mode {
            Mode::Diff => {
                let m = self.diff.model();
                if m.rows.is_empty() {
                    return (t("diff-status-empty"), String::new());
                }
                if m.is_identical() {
                    return (t("diff-status-identical"), String::new());
                }
                let position = self
                    .diff
                    .active_block()
                    .map(|block| (block + 1, m.blocks.len()));
                (
                    t_args(
                        "diff-status-counts",
                        &[
                            ("added", count(m.added)),
                            ("removed", count(m.removed)),
                            ("modified", count(m.modified)),
                        ],
                    ),
                    position_label(position),
                )
            }
            Mode::Merge => {
                let m = self.merge.model();
                (
                    t_args(
                        "merge-status-changes",
                        &[
                            ("count", count(m.change_count())),
                            ("resolved", count(m.resolved_count())),
                        ],
                    ),
                    position_label(self.merge.active_change()),
                )
            }
        }
    }

    fn drain_commands(&mut self) {
        while let Some(command) = self.chrome.as_ref().and_then(Chrome::take) {
            self.act(command.into());
        }
    }

    fn publish_header(&self) {
        if let Some(chrome) = &self.chrome {
            chrome.publish(self.header());
        }
    }

    fn header(&self) -> Header {
        let changes = self.load_error.is_none()
            && match self.mode {
                Mode::Diff => !self.diff.model().blocks.is_empty(),
                Mode::Merge => self.merge.model().change_count() > 0,
            };
        Header {
            mode: self.mode,
            changes,
            compact: self.compact,
        }
    }

    pub(crate) fn attach_chrome(&mut self, chrome: Chrome) {
        chrome.publish(self.header());
        self.chrome = Some(chrome);
    }

    /// The app before it is presented the document: empty panes and the
    /// "Open files" / "Load sample" toolbar actions.
    pub fn new() -> Self {
        Self {
            mode: Mode::Diff,
            diff: DiffView::new(),
            merge: MergeView::new(),
            rect: Rect::ZERO,
            diff_node: None,
            merge_node: None,
            chrome: None,
            status_h: status_bar::height(&Theme::dark()),
            compact: false,
            portal_rx: None,
            portal_contents_rx: None,
            pending_left: None,
            open_reply: None,
            load_error: None,
            invalidation: ViewInvalidation {
                paint: true,
                structure: false,
            },
            dispatcher: None,
            presented: None,
            shots_status: false,
        }
    }

    /// Show the document's state. A comparison or merge opened since the
    /// last state is shown in its mode; the first state opens on the
    /// comparison unless only a merge is open.
    pub(crate) fn present_state(&mut self, state: &MergeState) {
        let comparison_changed = self
            .presented
            .as_ref()
            .is_none_or(|(comparison, _)| *comparison != state.comparison);
        let merge_changed = self
            .presented
            .as_ref()
            .is_none_or(|(_, merge)| *merge != state.merge);
        if comparison_changed {
            match &state.comparison {
                Some(c) => self
                    .diff
                    .set_inputs(&c.left_label, &c.left, &c.right_label, &c.right),
                None => self.diff.set_inputs("", "", "", ""),
            }
        }
        self.merge.present(state);
        let mode = if self.presented.is_none() {
            Some(if state.comparison.is_none() && state.merge.is_some() {
                Mode::Merge
            } else {
                Mode::Diff
            })
        } else {
            match (comparison_changed, merge_changed) {
                (true, false) if state.comparison.is_some() => Some(Mode::Diff),
                (false, true) if state.merge.is_some() => Some(Mode::Merge),
                _ => None,
            }
        };
        if let Some(mode) = mode {
            self.show(mode);
        }
        self.presented = Some((state.comparison.clone(), state.merge.clone()));
        self.invalidation.paint = true;
    }

    pub(crate) fn attach_waker(&mut self, wake: ui::WakeHandle) {
        self.diff.set_waker(wake);
    }

    /// Submit edits and effects through `dispatcher`.
    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Diffmerge>) {
        self.merge
            .attach(crate::resource::merged_dispatcher(&dispatcher));
        self.merge.set_read_only(dispatcher.read_only());
        self.dispatcher = Some(dispatcher);
    }

    pub(crate) fn set_read_only(&mut self, read_only: bool) {
        self.merge.set_read_only(read_only);
    }

    /// Stage a screenshot scenario's view state.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("diff" | "empty") => self.show(Mode::Diff),
            Some("error") => {
                self.show(Mode::Diff);
                self.load_error = Some(SHOT_LOAD_ERROR.into());
                self.arm_status_bubble_for_shots();
            }
            Some("status") => {
                self.show(Mode::Merge);
                self.arm_status_bubble_for_shots();
            }
            _ => self.show(Mode::Merge),
        }
    }

    fn show(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.invalidation.structure = true;
        }
        self.invalidation.paint = true;
        self.publish_header();
    }

    /// Answer one of the app's effects, from a toolbar button, the palette,
    /// an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: DiffmergeEffect,
        cx: &mut EffectCx<Diffmerge, Self>,
    ) -> EffectAnswer {
        match effect {
            DiffmergeEffect::ShowDiff
            | DiffmergeEffect::ShowMerge
            | DiffmergeEffect::PreviousChange
            | DiffmergeEffect::NextChange
            | DiffmergeEffect::Show { .. } => {
                self.answer_view(effect)?;
                Ok(Effected::done())
            }
            DiffmergeEffect::LoadSample => {
                let dispatcher = cx.dispatcher();
                dispatcher.group(|| {
                    for action in crate::actions::sample_actions() {
                        dispatcher.apply(&action);
                    }
                });
                self.pending_left = None;
                self.load_error = None;
                self.invalidation.paint = true;
                Ok(Effected::done())
            }
            DiffmergeEffect::OpenFiles {
                left: Some(left),
                right: Some(right),
            } => Ok(cx.thread(
                move |_| -> Result<DiffmergeAction, String> {
                    let (left_label, left) = label_and_text(Path::new(&left))?;
                    let (right_label, right) = label_and_text(Path::new(&right))?;
                    Ok(DiffmergeAction::OpenComparison {
                        left_label,
                        left,
                        right_label,
                        right,
                    })
                },
                |app: &mut Self, opened, cx| match opened {
                    Ok(action) => {
                        app.load_error = None;
                        cx.apply(&action);
                        app.show(Mode::Diff);
                        Ok(Effected::done())
                    }
                    Err(reason) => {
                        app.load_error = Some(reason.clone());
                        app.invalidation.paint = true;
                        Err(reason)
                    }
                },
            )),
            DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            } => {
                if self.choosing() {
                    return Err("another file operation is already running".into());
                }
                self.request_open_via_portal();
                self.open_reply = Some(cx.reply());
                Ok(Effected::Pending)
            }
            DiffmergeEffect::OpenFiles { .. } => {
                Err("left and right paths must be supplied together".into())
            }
        }
    }

    /// Whether a file chooser is open.
    fn choosing(&self) -> bool {
        self.portal_rx.is_some() || self.portal_contents_rx.is_some() || self.open_reply.is_some()
    }

    /// `--shots`: force the floating status bubble fully visible so a single
    /// headless frame captures it (no live pointer hovers the reveal zone).
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
    }

    fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (self.rect.y1 - self.status_h).max(self.rect.y0),
        )
    }

    /// Give both views the content rect. Called from `apply_layout` and again
    /// from `draw` when the measured toolbar height changed, so the views are
    /// never painted a frame behind the band above them.
    fn place_views(&mut self) {
        let content = self.content_rect();
        self.diff.place(content);
        self.merge.place(content);
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::ModeDiff => self.show(Mode::Diff),
            Action::ModeMerge => self.show(Mode::Merge),
            Action::PrevChange | Action::NextChange => {
                let forward = action == Action::NextChange;
                match self.mode {
                    Mode::Diff => {
                        self.diff.navigate(forward);
                    }
                    Mode::Merge => {
                        self.merge.navigate(forward);
                    }
                }
                self.invalidation.paint = true;
            }
            Action::Open => self.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            }),
            Action::LoadSample => self.effect(&DiffmergeEffect::LoadSample),
        }
    }

    /// Run one of the app's effects through the same door the palette uses.
    fn effect(&mut self, effect: &DiffmergeEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
        self.invalidation.paint = true;
    }

    /// Submit the resolution a chip, a key or an accessibility action just
    /// queued on the merge panel — the same action the palette's row builds.
    fn submit_resolution(&mut self) {
        if let Some(action) = self.merge.take_pending_action()
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.apply(&action);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_mode_for_test(&mut self, mode: Mode) {
        self.show(mode);
    }

    /// Ask the host desktop's file chooser to open a file to compare. Per HIG
    /// §5 the app never draws its own picker: the portal round-trip runs on a
    /// worker thread ([`ui::portal::PickRequest`]) and the chosen path
    /// comes back over a channel drained each frame in [`Self::drain_portal`].
    /// A diff needs two files, so the drain re-fires this once for the right
    /// file after the left is picked.
    fn request_open_via_portal(&mut self) {
        self.load_error = None;
        if native_paths() {
            self.portal_rx = Some(diff_open_request().send());
        } else {
            self.portal_contents_rx = Some(diff_open_request().send_contents());
        }
        self.invalidation.paint = true;
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]. Loads the chosen file (stashing the first of the pair
    /// and re-opening the chooser for the second), records a failure for the
    /// status bubble, and clears the in-flight channel. Never blocks — a
    /// still pending picker leaves the channel installed for the next frame.
    fn drain_portal(&mut self) {
        if self
            .open_reply
            .as_ref()
            .is_some_and(EffectReply::is_cancelled)
        {
            self.portal_rx = None;
            self.portal_contents_rx = None;
            self.pending_left = None;
            self.open_reply = None;
            self.invalidation.paint = true;
            return;
        }
        if let Some(rx) = self.portal_contents_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.portal_contents_rx = None;
                    let outcome = match outcome {
                        ContentsOutcome::Picked(files) => match files.into_iter().next() {
                            Some(file) => Pick::Opened(
                                String::from_utf8(file.bytes)
                                    .map(|text| (file.name.clone(), text))
                                    .map_err(|_| format!("{}: not UTF-8 text", file.name)),
                            ),
                            None => Pick::Cancelled,
                        },
                        ContentsOutcome::Cancelled => Pick::Cancelled,
                        ContentsOutcome::Failed(reason) => Pick::Failed(reason),
                    };
                    self.apply_pick(outcome);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.portal_contents_rx = None;
                    self.apply_pick(Pick::Failed("file picker disconnected".into()));
                }
            }
        }
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                let outcome = match outcome {
                    PortalOutcome::Opened(path) => Pick::Opened(label_and_text(&path)),
                    PortalOutcome::Cancelled => Pick::Cancelled,
                    PortalOutcome::Failed(reason) => Pick::Failed(reason),
                };
                self.apply_pick(outcome);
            }
            // Still waiting on the host dialog.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                self.apply_pick(Pick::Failed("file picker disconnected".into()));
            }
        }
    }

    /// Answer the open-files effect a chooser was answering.
    fn finish_open(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.open_reply.take() {
            reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    /// Apply one chooser answer to the two-file selection.
    fn apply_pick(&mut self, outcome: Pick) {
        match outcome {
            Pick::Opened(Ok((label, text))) => {
                // A `true` return means the left file just landed and the
                // right one is still needed — ask for it.
                if self.consume_text(label, text) {
                    self.request_open_via_portal();
                } else {
                    self.finish_open(Ok(()));
                }
            }
            Pick::Opened(Err(reason)) => {
                self.pending_left = None;
                self.load_error = Some(reason.clone());
                self.finish_open(Err(reason));
            }
            // A cancel abandons an in-progress two-file selection.
            Pick::Cancelled => {
                self.pending_left = None;
                self.finish_open(Err("file selection cancelled".into()));
            }
            Pick::Failed(reason) => {
                self.pending_left = None;
                self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                self.finish_open(Err(reason));
            }
        }
        self.invalidation.paint = true;
    }

    /// Consume one chosen file. The first file of a pair is stashed and
    /// `true` is returned so the caller re-opens the chooser for the second;
    /// the second file opens the comparison and switches to Diff mode,
    /// returning `false`.
    fn consume_text(&mut self, label: String, content: String) -> bool {
        match self.pending_left.take() {
            None => {
                self.pending_left = Some((label, content));
                true
            }
            Some((left_label, left)) => {
                if let Some(dispatcher) = &self.dispatcher {
                    dispatcher.apply(&DiffmergeAction::OpenComparison {
                        left_label,
                        left,
                        right_label: label,
                        right: content,
                    });
                }
                self.show(Mode::Diff);
                false
            }
        }
    }

    /// The one-line summary the status bubble shows: a host-portal load error
    /// takes over until the next successful open clears it, otherwise the
    /// diff-mode change counts or the merge-mode hunk/conflict counts.
    fn status_text(&self) -> String {
        if let Some(err) = &self.load_error {
            return err.clone();
        }
        match self.mode {
            Mode::Diff => {
                let m = self.diff.model();
                let active = self
                    .diff
                    .active_block()
                    .map(|b| format!(" Change {}/{}.", b + 1, m.blocks.len().max(1)))
                    .unwrap_or_default();
                format!(
                    "{} additions, {} deletions and {} modifications in {} change block(s).{}",
                    m.added,
                    m.removed,
                    m.modified,
                    m.blocks.len(),
                    active
                )
            }
            Mode::Merge => {
                let m = self.merge.model();
                format!(
                    "{} change hunk(s)   {} resolved   {} conflict(s) remaining",
                    m.change_count(),
                    m.resolved_count(),
                    m.conflict_count(),
                )
            }
        }
    }

    /// Keyboard shortcuts: F8/F7 or n/p for next/prev change, 1/2 for mode.
    /// Returns `true` if the event was consumed.
    fn handle_keyboard_shortcut(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };

        match logical_key {
            Key::Character(ch) => match ch.as_str() {
                "n" | "N" => {
                    self.act(Action::NextChange);
                    true
                }
                "p" | "P" => {
                    self.act(Action::PrevChange);
                    true
                }
                "1" => {
                    self.act(Action::ModeDiff);
                    true
                }
                "2" => {
                    self.act(Action::ModeMerge);
                    true
                }
                _ => false,
            },
            Key::Named(NamedKey::F8) => {
                self.act(Action::NextChange);
                true
            }
            Key::Named(NamedKey::F7) => {
                self.act(Action::PrevChange);
                true
            }
            _ => false,
        }
    }

    // Header controls are owned by the shared NavShell.
}

impl Default for DiffMergeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for DiffMergeApp {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if self.diff.collect_highlights() {
            self.invalidation.paint = true;
        }
        self.drain_portal();
        self.drain_commands();
        self.publish_header();
        match self.mode {
            Mode::Diff => self.diff.sync(state, path),
            Mode::Merge => self.merge.sync(state, path),
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let diff_node = self.diff.build_layout(taffy, theme, node_map);
        let merge_node = self.merge.build_layout(taffy, theme, node_map);
        self.diff_node = Some(diff_node);
        self.merge_node = Some(merge_node);

        // Only the active view participates in the flex content area; the other
        // is laid out off-band (we drive its rect manually in apply_layout).
        let content_child = match self.mode {
            Mode::Diff => diff_node,
            Mode::Merge => merge_node,
        };

        // `new_with_children` only fails when a child belongs to a different
        // tree; `content_child` and `content` were just built in this one.
        #[allow(
            clippy::unwrap_used,
            reason = "content_child was just created in this same tree"
        )]
        let content = taffy
            .new_with_children(
                Style {
                    flex_grow: 1.0,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[content_child],
            )
            .unwrap();

        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[content], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // Drive both views to the content rect directly so mode switches
        // don't require a taffy rebuild to position correctly, and so the
        // views end at the window bottom rather than a toolbar's height below
        // it (the taffy content node is the full root height).
        self.place_views();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The window ground is the framework's; reserve the status bar first
        // so the views are placed above the band this frame will paint.
        // An error replaces the comparison and its status; no empty band is
        // left behind.
        let status_h = if self.load_error.is_some() {
            0.0
        } else {
            status_bar::height(theme)
        };
        #[expect(
            clippy::float_cmp,
            reason = "change detection against the cached measurement: any bit difference means the views must be re-placed"
        )]
        let status_changed = status_h != self.status_h;
        if status_changed {
            self.status_h = status_h;
            self.place_views();
        }
        let content = self.content_rect();
        let compact = self.rect.width() < f64::from(theme.tokens.metrics.compact_breakpoint);
        if compact != self.compact {
            self.compact = compact;
            self.publish_header();
        }

        if let Some(reason) = &self.load_error {
            let mut ctx = PaintCtx::new(scene, transform, tr, theme);
            Self::draw_error(&mut ctx, content, reason);
        } else {
            match self.mode {
                Mode::Diff => self.diff.draw(scene, transform, tr, theme, presence),
                Mode::Merge => self.merge.draw(scene, transform, tr, theme, presence),
            }
        }

        if self.load_error.is_none() {
            let (_, bar) = status_bar::split(self.rect, theme);
            let (left, right) = self.status_readouts();
            status_bar::draw(scene, transform, tr, theme, bar, &left, &right);
        }
        self.invalidation.paint = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.merge.merged_editor_focused() && self.handle_keyboard_shortcut(event) {
            *ctx.handled = true;
            return;
        }
        match self.mode {
            Mode::Diff => {
                if let Some(n) = self.diff_node {
                    self.diff
                        .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                }
            }
            Mode::Merge => {
                if let Some(n) = self.merge_node {
                    self.merge
                        .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                }
                self.submit_resolution();
            }
        }
    }

    fn name(&self) -> &'static str {
        "DiffMergeApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("diffmerge.app")
    }

    // The shared shell owns header chrome, while the active comparison view
    // owns its own semantic controls. Exposing that live child is what lets the
    // compact source switcher reach the actual Tilo accessibility tree rather
    // than only being available to pointer users.
    fn children(&self) -> Vec<&dyn Widget> {
        match self.mode {
            Mode::Diff => vec![&self.diff],
            Mode::Merge => vec![&self.merge],
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match self.mode {
            Mode::Diff => vec![&mut self.diff],
            Mode::Merge => vec![&mut self.merge],
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};

        let mut children = Vec::new();
        // The active view is one of this widget's children and publishes
        // itself; its controls are published here, where their actions are
        // handled.
        children.extend(match self.mode {
            Mode::Diff => self.diff.access_controls(),
            Mode::Merge => self.merge.access_controls(),
        });
        if self.mode == Mode::Diff && self.diff.model().rows.is_empty() {
            children.push(
                AccessNode::new(Role::Paragraph)
                    .with_key(ui::a11y::hash_key(&"diffmerge.empty"))
                    .with_name("No files loaded")
                    .with_description("Use Open files to start a local comparison."),
            );
        }
        if let Some(error) = &self.load_error {
            children.push(
                AccessNode::new(Role::Paragraph)
                    .with_key(ui::a11y::hash_key(&"diffmerge.load-error"))
                    .with_name("File loading failed")
                    .with_description(error),
            );
        }
        children.push(
            AccessNode::new(Role::Paragraph)
                .with_key(ui::a11y::hash_key(&"diffmerge.status"))
                .with_name("Comparison status")
                .with_description(self.status_text()),
        );
        Some(
            AccessNode::new(Role::Group)
                .with_name("Diff and Merge")
                .with_description("Compare local files or resolve a shared three-way merge")
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let _ = (state, path);
        if !matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus) {
            return false;
        }
        if let Some((block, fingerprint)) = target
            .strip_prefix("diffmerge.diff-block:")
            .and_then(|id| id.split_once(':'))
            .and_then(|(index, fingerprint)| {
                index
                    .parse::<usize>()
                    .ok()
                    .map(|index| (index, fingerprint))
            })
        {
            if self.mode != Mode::Diff || !self.diff.show_block(block, fingerprint) {
                return false;
            }
            self.invalidation.paint = true;
            return true;
        }
        if let Some(show_right) = match target {
            "diffmerge.diff-source:left" => Some(false),
            "diffmerge.diff-source:right" => Some(true),
            _ => None,
        } {
            if self.mode != Mode::Diff {
                return false;
            }
            self.diff.select_compact_side(show_right);
            self.invalidation.paint = true;
            return true;
        }
        if let Some((hunk, choice)) = target
            .strip_prefix("diffmerge.merge.resolve:")
            .and_then(|value| value.split_once(':'))
            .and_then(|(hunk, choice)| {
                let hunk = hunk.parse::<usize>().ok()?;
                let choice = match choice {
                    "ours" => crate::actions::HunkChoice::Ours,
                    "theirs" => crate::actions::HunkChoice::Theirs,
                    "both" => crate::actions::HunkChoice::Both,
                    _ => return None,
                };
                Some((hunk, choice))
            })
        {
            if self.mode != Mode::Merge || !self.merge.can_resolve(hunk) {
                return false;
            }
            if matches!(action, ui::a11y::Action::Activate) {
                self.merge.resolve(hunk, choice);
                self.submit_resolution();
            }
            self.invalidation.paint = true;
            return true;
        }
        false
    }

    fn tooltip(&self) -> Option<&str> {
        None
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Mode switches swap which child is in the flex content, so signal a
        // rebuild when the mode flips. We detect this by tracking a one-shot.
        std::mem::take(&mut self.invalidation.structure)
    }

    fn needs_redraw(&self) -> bool {
        self.invalidation.paint || self.diff.needs_redraw() || self.merge.needs_redraw()
    }

    /// Keep ticking while a host-portal open is in flight so the off-thread
    /// result is drained the moment it arrives. Idle otherwise (the `--shots`
    /// / offscreen path never has a picker pending, so it settles to zero
    /// frames).
    fn needs_animation_frame(&self) -> bool {
        self.choosing()
    }

    fn mark_drawn(&mut self) {
        self.invalidation.paint = false;
        self.diff.mark_drawn();
        self.merge.mark_drawn();
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

impl DiffMergeApp {
    /// Answer a view operation: move around what is open.
    /// Move around what is open, as a view effect asks.
    pub(crate) fn answer_view(&mut self, effect: DiffmergeEffect) -> Result<(), String> {
        self.act(match effect {
            DiffmergeEffect::ShowDiff => Action::ModeDiff,
            DiffmergeEffect::ShowMerge => Action::ModeMerge,
            DiffmergeEffect::PreviousChange => Action::PrevChange,
            DiffmergeEffect::NextChange => Action::NextChange,
            DiffmergeEffect::Show { block, fingerprint } => {
                let Ok(block) = usize::try_from(block) else {
                    return Err("this change target is no longer available".into());
                };
                if !self.diff.show_block(block, &fingerprint) {
                    return Err(
                        "the comparison changed; reopen this change from the current results"
                            .into(),
                    );
                }
                self.invalidation.paint = true;
                return Ok(());
            }
            DiffmergeEffect::OpenFiles { .. } | DiffmergeEffect::LoadSample => {
                return Err("not a view of what is open".into());
            }
        });
        Ok(())
    }
}

/// What the chooser answered.
enum Pick {
    /// A file's label and text, or why it could not be read.
    Opened(Result<(String, String), String>),
    Cancelled,
    Failed(String),
}

fn label_and_text(path: &Path) -> Result<(String, String), String> {
    let label = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    Ok((label, read_text(path)?))
}

fn read_text(path: &Path) -> Result<String, String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let (dir, name) = tilo_files::file_at(path).map_err(|e| failed(&e))?;
    let bytes = tilo_files::blocking::read(&dir, &name).map_err(|e| failed(&e))?;
    String::from_utf8(bytes).map_err(|e| failed(&e))
}

fn position_label(position: Option<(usize, usize)>) -> String {
    position.map_or_else(String::new, |(current, total)| {
        t_args(
            "diff-status-position",
            &[("current", count(current)), ("total", count(total))],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::MergeDoc;
    use crate::resource::DiffmergeShell;
    use tilo_actions::Action as _;
    use ui::framework::testing::TestApp;
    use ui::headless::Framed;
    use ui::tilo_resource::Resource as _;
    use winit::event::MouseButton;

    /// A document as a new workspace starts.
    fn new_document() -> ui::State {
        let mut state = ui::State::new();
        Diffmerge::initialize(&mut state.doc).unwrap();
        state
    }

    /// The bundled samples, opened in a document through their actions.
    fn sample_document() -> ui::State {
        let mut state = new_document();
        for action in crate::actions::sample_actions() {
            action.apply(&mut state.doc).unwrap();
        }
        state
    }

    /// The app presented `state` and staged for `scenario`, as a capture
    /// shows it.
    fn presented(state: ui::State, scenario: Option<&str>) -> (DiffMergeApp, ui::State) {
        let mut app = DiffMergeApp::new();
        app.present_state(&MergeState::from_doc(&state.doc));
        app.stage_scenario(scenario);
        (app, state)
    }

    /// The samples open on the three-way merge.
    fn demo() -> (DiffMergeApp, ui::State) {
        presented(sample_document(), None)
    }

    /// A screenshot scenario's document and view state.
    fn staged(scenario: Option<&str>) -> (DiffMergeApp, ui::State) {
        let state = if scenario == Some("empty") {
            new_document()
        } else {
            sample_document()
        };
        presented(state, scenario)
    }

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("diffmerge-open-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn sample_app() -> TestApp<DiffmergeShell> {
        let mut app = TestApp::<DiffmergeShell>::new().unwrap();
        app.effect(&DiffmergeEffect::LoadSample).unwrap();
        app.settle();
        app
    }

    fn header_controls(app: &mut TestApp<DiffmergeShell>) -> Vec<ui::headless::Control> {
        let controls = app.harness().controls();
        [
            "Diff",
            "3-way merge",
            "Previous change",
            "Next change",
            "Load sample",
            "Open files",
        ]
        .into_iter()
        .map(|name| {
            controls
                .iter()
                .find(|control| {
                    control.name.as_deref().is_some_and(|label| {
                        label == name
                            || (name == "3-way merge" && label == "Merge")
                            || (name == "Load sample" && label == "Sample")
                    })
                })
                .unwrap_or_else(|| panic!("missing header control {name}: {controls:?}"))
                .clone()
        })
        .collect()
    }
    fn first_conflict(state: &MergeState) -> usize {
        state
            .model
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .expect("the sample merge has a conflict")
    }

    /// The bounds of the drawn semantic node whose action target is `target`.
    fn target_bounds(app: &mut TestApp<DiffmergeShell>, target: &str) -> Option<Rect> {
        fn in_access(node: &ui::a11y::AccessNode, target: &str) -> Option<Rect> {
            if node.action_target.as_deref() == Some(target) {
                return node.bounds.map(|b| Rect::new(b.x0, b.y0, b.x1, b.y1));
            }
            node.children
                .iter()
                .find_map(|child| in_access(child, target))
        }
        fn in_node(node: &ui::headless::InspectionNode, target: &str) -> Option<Rect> {
            node.semantics
                .as_ref()
                .and_then(|semantics| in_access(semantics, target))
                .or_else(|| {
                    node.children
                        .iter()
                        .find_map(|child| in_node(child, target))
                })
        }
        in_node(&app.harness().inspection().root, target)
    }

    /// Opening files is one effect, whoever asks: two paths are read and
    /// opened as a comparison; without paths the host's chooser is asked
    /// twice, and a cancelled or failed chooser opens nothing and says why.
    #[test]
    fn open_files_reads_paths_or_asks_the_host_chooser() {
        let mut app = TestApp::<DiffmergeShell>::new().unwrap();
        let left = write_temp("Grün links.txt", "Grün\nshared\n");
        let right = write_temp("Blau rechts.txt", "Blau\nshared\nadded\n");
        app.effect(&DiffmergeEffect::OpenFiles {
            left: Some(left.display().to_string()),
            right: Some(right.display().to_string()),
        })
        .unwrap();
        app.settle();
        let comparison = app.state().comparison.expect("the pair is open");
        assert_eq!(comparison.left_label, "Grün links.txt");
        assert_eq!(comparison.right, "Blau\nshared\nadded\n");
        let model = comparison.model();
        assert_eq!((model.left_lines, model.right_lines), (2, 3));

        let half = app.effect(&DiffmergeEffect::OpenFiles {
            left: Some(left.display().to_string()),
            right: None,
        });
        assert!(half.is_err(), "both paths or neither");

        let (services, recorder) = ui::host::HostServices::recording();
        let chosen_left = write_temp("left.txt", "alpha\nbeta\ngamma\n");
        let chosen_right = write_temp("right.txt", "alpha\nBETA\ngamma\ndelta\n");
        recorder.respond_to_pick(PortalOutcome::Opened(chosen_left.clone()));
        recorder.respond_to_pick(PortalOutcome::Opened(chosen_right.clone()));
        ui::host::scoped(&services, || {
            app.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            })
        })
        .expect("both files were chosen");
        app.settle();
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 2, "one chooser per file");
        assert_eq!(picks[0].title(), "Open a file to compare");
        let comparison = app.state().comparison.unwrap();
        assert_eq!(
            (
                comparison.left_label.as_str(),
                comparison.right_label.as_str()
            ),
            ("left.txt", "right.txt")
        );

        // The recording host cancels once its queued answers run out.
        let cancelled = ui::host::scoped(&services, || {
            app.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            })
        });
        assert!(cancelled.is_err(), "a cancelled chooser opens nothing");
        assert_eq!(app.state().comparison.unwrap().left_label, "left.txt");

        recorder.respond_to_pick(PortalOutcome::Failed("no portal".into()));
        let failed = ui::host::scoped(&services, || {
            app.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            })
        });
        assert!(format!("{failed:?}").contains("no portal"), "{failed:?}");
        app.settle();
        let tree = app.harness().inspection_json().unwrap();
        assert!(tree.contains("File loading failed"), "the failure is shown");

        for path in [left, right, chosen_left, chosen_right] {
            std::fs::remove_dir_all(path.parent().unwrap()).ok();
        }
    }

    /// Taking a side is one action whichever way it is asked for: the
    /// palette's conflict row or a click on the drawn chip. Each is one undo
    /// step, and typing in the merged
    /// result afterwards is an edit of the document's text that stops sides
    /// from being taken.
    #[test]
    fn hunk_resolution_is_one_action_from_palette_pointer_and_a11y() {
        let mut app = sample_app();
        let state = app.state();
        assert!(state.composed && state.merge.is_some());
        let hunk = first_conflict(&state);
        let before = state.merged;

        // The palette: the conflict's row.
        let surface = app.agent_surface();
        let conflict = surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                row["resource_kind"] == "diffmerge.conflict" && row["action"] == "take-theirs"
            })
            .and_then(|row| row["resource"].as_str())
            .expect("the conflict is published")
            .to_owned();
        app.call_on(&conflict, "take-theirs", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_ne!(app.state().merged, before);
        let resolved = app.read_document(MergeDoc::resolutions);
        assert!(
            matches!(resolved.as_slice(), [(_, choice)] if choice == "theirs"),
            "the row resolved its conflict: {resolved:?}"
        );
        assert!(app.undo(), "one undo step");
        app.settle();
        assert_eq!(app.state().merged, before, "undo took exactly it back");

        // A click on the drawn chip, in the merge view.
        app.call("show-merge", serde_json::json!({})).unwrap();
        let theirs = target_bounds(&mut app, &format!("diffmerge.merge.resolve:{hunk}:theirs"))
            .expect("the take-theirs chip is drawn");
        app.harness().click_at(theirs.center().x, theirs.center().y);
        app.settle();
        let state = app.state();
        assert_eq!(
            app.read_document(MergeDoc::resolutions),
            vec![(hunk, "theirs".to_string())],
            "the click resolved that hunk"
        );
        assert_ne!(state.merged, before);
        assert!(app.undo());
        app.settle();

        // Typing in the merged result.
        // The merged result fills the bottom of the window.
        let [width, height] = app.harness().inspection().viewport;
        app.harness()
            .click_at(f64::from(width) / 2.0, f64::from(height) - 40.0);
        app.harness().type_text("Z");
        app.settle();
        let state = app.state();
        assert!(state.merged.contains('Z'), "{:?}", state.merged);
        assert!(!state.composed, "a hand edit stops sides from being taken");
        assert!(
            target_bounds(&mut app, &format!("diffmerge.merge.resolve:{hunk}:ours")).is_none(),
            "no chip is offered over a hand edit"
        );
    }

    /// Each screenshot scenario stages through the framework: the samples
    /// through their actions, the mode, error and status through the view.
    #[test]
    fn scenarios_stage_through_actions_and_view_state() {
        let empty = TestApp::<DiffmergeShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().comparison.is_none() && empty.state().merge.is_none());
        for scenario in ["diff", "merge", "error", "status"] {
            let mut app = TestApp::<DiffmergeShell>::capture(Some(scenario)).unwrap();
            let state = app.state();
            assert!(
                state.comparison.is_some() && state.merge.is_some(),
                "{scenario}"
            );
            let tree = app.harness().inspection_json().unwrap();
            match scenario {
                "diff" => assert!(tree.contains("Diff between"), "{scenario}"),
                "merge" | "status" => assert!(tree.contains("Three-way merge"), "{scenario}"),
                _ => assert!(tree.contains("File loading failed"), "{scenario}"),
            }
        }
    }

    /// Mount `app` over the case's `state` in the production frame host and run
    /// one frame, which lays out and draws the merge panel (populating the
    /// resolution-chip hit rects).
    fn merge_frame(
        app: DiffMergeApp,
        state: ui::State,
        viewport: (f32, f32),
    ) -> Framed<DiffMergeApp> {
        frame_scaled(app, state, viewport, 1.0)
    }

    /// [`merge_frame`] at `text_scale`, the way the launcher's `--text-scale`
    /// does: the theme's type tokens and the renderer's scale move together.
    fn frame_scaled(
        app: DiffMergeApp,
        state: ui::State,
        viewport: (f32, f32),
        text_scale: f32,
    ) -> Framed<DiffMergeApp> {
        Framed::new(
            state,
            app,
            ui::headless::viewport(viewport),
            Theme::dark().with_text_scale(text_scale),
        )
    }

    /// Rebuild and run the next frame of a mounted app at a new size.
    fn rerender_merge_frame(app: &mut Framed<DiffMergeApp>, viewport: (f32, f32)) {
        Framed::harness_mut(app)
            .relayout_themed(ui::headless::viewport(viewport), Theme::dark())
            .expect("diffmerge frame lays out");
    }

    fn keyboard_event(key: Key) -> WindowEvent {
        use winit::event::{DeviceId, KeyEvent};
        use winit::keyboard::{KeyLocation, PhysicalKey};

        // `KeyEvent` has an opaque platform payload. The public fields below
        // are exactly the ones the widget reads; this mirrors the toolkit's
        // own headless keyboard helpers.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    fn selected_compact_source(app: &DiffMergeApp) -> String {
        let node = Widget::accessibility(&app.merge).expect("merge is exposed to assistive tech");
        node.children
            .iter()
            .find(|child| child.state.checked == Some(true))
            .and_then(|child| child.name.clone())
            .expect("one compact source is selected")
    }

    /// Phone-width layout: the three-way merge and the two-way diff present a
    /// one-source switcher, the toolbar stays on screen without overlap, and a long
    /// error bubble stays inside the viewport. Fresh app per phase.
    #[test]
    fn phone_width_keeps_controls_onscreen_with_one_source_switchers() {
        phone_merge_uses_a_full_width_source_switcher();
        phone_toolbar_keeps_every_control_visible_without_overlap();
        phone_diff_uses_an_accessible_one_source_switcher();
        narrow_status_bubble_stays_within_the_viewport_for_long_errors();
    }

    /// At phone width, a three-way merge intentionally presents one full-width
    /// source pane plus an explicit Base/Ours/Theirs switcher. This guards
    /// against a future layout change quietly restoring three unreadable
    /// columns at 390px.
    fn phone_merge_uses_a_full_width_source_switcher() {
        let (app, state) = demo();
        let mut app = merge_frame(app, state, (390.0, 760.0));

        let hits = app.merge.compact_source_hits();
        assert_eq!(hits.len(), 3, "phone has Base/Ours/Theirs source controls");
        assert_eq!(
            hits.iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["Base", "Ours", "Theirs"]
        );
        for (rect, _) in &hits {
            assert!(
                rect.width() >= 110.0,
                "each source target stays finger-sized: {rect:?}"
            );
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= 390.0,
                "source target stays onscreen: {rect:?}"
            );
        }
        assert!(hits.windows(2).all(|pair| pair[0].0.x1 <= pair[1].0.x0));

        let semantic = Widget::accessibility(&app.merge).expect("merge semantics");
        assert_eq!(
            semantic
                .children
                .iter()
                .filter(|child| child
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Show ")))
                .count(),
            3,
            "the source switcher remains accessible alongside resolution controls"
        );
        assert_eq!(selected_compact_source(&app), "Show Ours source");

        // At tablet width the selector disappears because all three panes are
        // deliberately visible side by side.
        rerender_merge_frame(
            &mut app,
            (
                Theme::dark().tokens.metrics.compact_breakpoint + 160.0,
                760.0,
            ),
        );
        assert!(app.merge.compact_source_hits().is_empty());
        assert!(
            Widget::accessibility(&app.merge)
                .expect("merge semantics")
                .children
                .iter()
                .all(|child| {
                    !child
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("Show "))
                })
        );
    }

    /// Modes and change navigation remain readable touch targets. The shared
    /// More menu keeps the file commands reachable without squeezing those
    /// labels to ellipses on a phone.
    fn phone_toolbar_keeps_every_control_visible_without_overlap() {
        let mut app = TestApp::<DiffmergeShell>::capture_at(Some("merge"), (390, 760)).unwrap();
        let controls = header_controls(&mut app);
        assert_eq!(controls.len(), 6, "two modes and four canonical actions");
        let mut rects: Vec<_> = controls
            .iter()
            .skip(2)
            .take(2)
            .chain(controls.iter().take(2))
            .map(|control| control.bounds.expect("painted touch target"))
            .collect();
        rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        assert!(
            rects
                .iter()
                .all(|rect| rect.x0 >= 0.0 && rect.x1 <= 390.0 && rect.height() >= 40.0),
            "{rects:?}"
        );
        assert!(
            rects.windows(2).all(|pair| pair[0].x1 <= pair[1].x0),
            "{rects:?}"
        );
        let more = app.harness().targets()["nav-shell-more-content"].center();
        app.harness().click_at(more.x, more.y);
        app.settle();
        let nodes = app.harness().a11y_nodes();
        for name in ["Sample", "Open files"] {
            let node = nodes
                .iter()
                .find(|node| {
                    node.1.label() == Some(name)
                        && node.1.supports_action(ui::accesskit::Action::Click)
                })
                .expect("the real More menu offers the file command");
            let rect = node.1.bounds().expect("menu command is painted");
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= 390.0 && rect.y0 >= 0.0 && rect.y1 <= 760.0,
                "{name}: {rect:?}"
            );
        }
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{audit:#?}");
    }

    /// Measured-text layout at 1x and 200%: toolbar controls, merge chips, merge
    /// rows and the diff gutter all follow the renderer. Fresh apps per phase.
    #[test]
    fn layout_metrics_follow_the_measured_text_at_every_scale() {
        toolbar_controls_fit_their_measured_labels_at_every_text_scale();
        merge_chips_stay_inside_their_pane();
        merge_rows_follow_the_scaled_line_height();
        diff_rows_and_gutter_follow_the_scaled_line_height();
    }

    /// Every toolbar control is sized from its measured label and centred on
    /// the measured bar, at 1× and at 200% text; the content starts below the
    /// bar that was actually painted.
    fn toolbar_controls_fit_their_measured_labels_at_every_text_scale() {
        let mut unscaled = None;
        for scale in [1.0f32, 2.0] {
            let mut app = TestApp::<DiffmergeShell>::capture_at(Some("merge"), (800, 700)).unwrap();
            app.harness()
                .set_theme(Theme::dark().with_text_scale(scale));
            app.harness().relayout();
            app.settle();
            let controls = header_controls(&mut app);
            let mut rects: Vec<_> = controls
                .iter()
                .map(|control| control.bounds.expect("painted header control"))
                .collect();
            let header = app
                .with_surface::<ui::widget::NavShell, _>(|shell| shell.geometry().header_height)
                .unwrap();
            for rect in &rects {
                assert!(rect.x0 >= 0.0 && rect.x1 <= 800.0, "{scale}x: {rect:?}");
                assert!(
                    (rect.center().y - header / 2.0).abs() < 0.01,
                    "{scale}x: {rect:?} header {header}"
                );
                assert!(rect.y1 <= header + 0.01, "{scale}x: {rect:?}");
            }
            rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            assert!(
                rects.windows(2).all(|pair| pair[0].x1 <= pair[1].x0),
                "{rects:?}"
            );
            app.with_surface::<DiffMergeApp, _>(|view| {
                let active = match view.mode {
                    Mode::Diff => view.diff.get_rect(),
                    Mode::Merge => view.merge.get_rect(),
                };
                assert!(
                    (active.y0 - header).abs() < 0.01,
                    "{active:?} header {header}"
                );
                assert!(
                    (active.y1 - view.content_rect().y1).abs() < 0.01,
                    "content fills its area above the status bar"
                );
            })
            .unwrap();
            let label_widths: Vec<_> = [0usize, 1, 4]
                .into_iter()
                .filter_map(|index| controls.get(index))
                .map(|control| control.bounds.unwrap().width())
                .collect();
            if let Some((previous_header, previous_widths)) = unscaled.take() {
                assert!(
                    header > previous_header,
                    "200% text grows the shared header"
                );
                assert!(
                    label_widths
                        .iter()
                        .zip(previous_widths)
                        .all(|(wide, narrow)| *wide > narrow),
                    "scaled text grows labelled controls"
                );
            } else {
                unscaled = Some((header, label_widths));
            }
        }
    }

    /// Every resolution chip paints inside the pane that owns it, at 800 and
    /// 1200 wide and at 200% text: chip widths are measured, and the long
    /// "Take …" labels fall back to the short form when they cannot fit.
    fn merge_chips_stay_inside_their_pane() {
        for (w, h, scale) in [
            (800.0f32, 700.0f32, 1.0f32),
            (1200.0, 800.0, 1.0),
            (800.0, 900.0, 2.0),
        ] {
            let (app, state) = demo();
            let app = frame_scaled(app, state, (w, h), scale);
            let pane = app.merge.get_rect();
            let hits = app.merge.chip_hits();
            assert!(
                !hits.is_empty(),
                "{w}x{h}@{scale}: the demo merge has chips"
            );
            let (_, _, chip_h) = app.merge.metrics_for_test();
            let compact = pane.width()
                < f64::from(
                    Theme::dark()
                        .with_text_scale(scale)
                        .tokens
                        .metrics
                        .compact_breakpoint,
                );
            let pane_x0 = if compact {
                pane.x0
            } else {
                pane.x0 + pane.width() * 2.0 / 3.0
            };
            for (rect, hunk, _) in &hits {
                assert!(
                    rect.x0 >= pane_x0 - 0.01,
                    "{w}x{h}@{scale}: chip for hunk {hunk} {rect:?} starts before its pane"
                );
                assert!(
                    rect.x1 <= pane.x1 - 8.0 + 0.01,
                    "{w}x{h}@{scale}: chip for hunk {hunk} {rect:?} runs past the pane edge"
                );
                assert!((rect.height() - chip_h).abs() < 0.01);
            }
        }
    }

    /// Row pitch, header band and chip band follow the renderer's measured
    /// line height, so 200% text does not overprint 19px rows.
    fn merge_rows_follow_the_scaled_line_height() {
        let (one, state) = demo();
        let one = frame_scaled(one, state, (1200.0, 800.0), 1.0);
        let (row_1, header_1, chip_1) = one.merge.metrics_for_test();
        let content_1 = one.merge.content_height_for_test();

        let (two, state) = demo();
        let two = frame_scaled(two, state, (1200.0, 800.0), 2.0);
        let (row_2, header_2, chip_2) = two.merge.metrics_for_test();
        let content_2 = two.merge.content_height_for_test();

        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let line_2 = f64::from(tr.measure("Ag", 12.5, None).1);
        assert!(
            row_2 >= line_2,
            "row pitch {row_2} shorter than the 2x line {line_2}"
        );
        assert!(row_2 > row_1 * 1.5 && header_2 > header_1 && chip_2 > chip_1);
        assert!(content_2 > content_1 * 1.5, "spans grow with the rows");
    }

    /// The two-way diff shares the same contract: rows, header and the
    /// line-number gutter grow with the measured text.
    fn diff_rows_and_gutter_follow_the_scaled_line_height() {
        let (mut one, state) = demo();
        one.act(Action::ModeDiff);
        let one = frame_scaled(one, state, (1200.0, 800.0), 1.0);
        let (row_1, header_1, gutter_1) = one.diff.metrics_for_test();

        let (mut two, state) = demo();
        two.act(Action::ModeDiff);
        let two = frame_scaled(two, state, (1200.0, 800.0), 2.0);
        let (row_2, header_2, gutter_2) = two.diff.metrics_for_test();

        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let line_2 = f64::from(tr.measure("Ag", 13.0, None).1);
        let digits_2 = f64::from(tr.measure("0000", 12.0, None).0);
        assert!(row_2 >= line_2 && row_2 > row_1 * 1.5);
        assert!(header_2 > header_1);
        assert!(
            gutter_2 >= digits_2 + 16.0 && gutter_2 > gutter_1,
            "gutter {gutter_2} cannot hold four 2x digits ({digits_2})"
        );
    }

    /// Input parity for the chrome: the compact source switcher by pointer, keyboard
    /// and a11y; toolbar semantics, focus and activation; Tab/Enter order; and the
    /// empty/error content explained to assistive tech. Fresh app per phase.
    #[test]
    fn controls_have_pointer_keyboard_and_semantic_parity() {
        compact_source_switcher_has_pointer_keyboard_and_a11y_parity();
        toolbar_controls_have_semantic_names_focus_geometry_and_activation();
        toolbar_keyboard_tab_and_enter_follow_the_painted_control_order();
        root_accessibility_explains_empty_and_error_content();
    }

    /// Pointer, keyboard, and assistive activation all select the same compact
    /// source state. The test drives the real event/semantic routes rather
    /// than calling the selection helper directly.
    fn compact_source_switcher_has_pointer_keyboard_and_a11y_parity() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Modifiers};

        let (app, state) = demo();
        let mut app = merge_frame(app, state, (390.0, 760.0));
        let (app, state) = Framed::split(&mut app);
        let theirs = app
            .merge
            .compact_source_hits()
            .into_iter()
            .find(|(_, name)| *name == "Theirs")
            .expect("Theirs selector")
            .0
            .center();
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.handle_event(
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(theirs.x, theirs.y),
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: ui::widget::Edits::lend(&mut state.doc),
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        app.handle_event(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: ui::widget::Edits::lend(&mut state.doc),
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(selected_compact_source(app), "Show Theirs source");

        handled = false;
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::ArrowLeft)),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: ui::widget::Edits::lend(&mut state.doc),
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(selected_compact_source(app), "Show Ours source");

        let theirs_key = ui::a11y::hash_key(&"diffmerge.source.theirs");
        assert!(Widget::perform_action_at(
            &mut app.merge,
            &ui::a11y::Action::Activate,
            &[theirs_key],
            &mut ui::widget::Edits::lend(&mut state.doc),
            &state.root,
        ));
        assert_eq!(selected_compact_source(app), "Show Theirs source");
    }

    fn toolbar_controls_have_semantic_names_focus_geometry_and_activation() {
        use ui::headless::ControlInput;
        let mut app = sample_app();
        let controls = header_controls(&mut app);
        let canvas = app
            .harness()
            .controls()
            .into_iter()
            .find(|control| {
                control.role == ui::accesskit::Role::Canvas
                    && control
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("Diff between"))
            })
            .expect("actual diff canvas is admitted");
        assert!(canvas.bounds.is_some());
        assert!(canvas.verbs.iter().any(|verb| verb == "focus"));
        app.harness()
            .operate_control(&canvas.id, &ControlInput::Focus)
            .unwrap();
        assert!(
            app.harness()
                .controls()
                .iter()
                .any(|control| control.id == canvas.id && control.focused)
        );
        assert_eq!(controls.len(), 6, "both modes and four actions");
        assert!(
            controls.iter().all(|control| control.bounds.is_some()
                && control.verbs.iter().any(|verb| verb == "activate")),
            "actual exported controls: {controls:?}"
        );
        let sample = controls
            .iter()
            .find(|control| control.name.as_deref() == Some("Load sample"))
            .unwrap();
        app.harness()
            .operate_control(&sample.id, &ControlInput::Focus)
            .unwrap();
        assert!(
            app.harness()
                .controls()
                .iter()
                .any(|control| control.id == sample.id && control.focused)
        );
        let heads = app.read_document(|doc| doc.clone().get_heads());
        let diff = controls
            .iter()
            .find(|control| control.name.as_deref() == Some("Diff"))
            .unwrap();
        app.harness()
            .operate_control(&diff.id, &ControlInput::Press)
            .unwrap();
        app.settle();
        assert_eq!(
            app.with_surface::<DiffMergeApp, _>(|view| view.mode),
            Some(Mode::Diff)
        );
        assert_eq!(
            app.read_document(|doc| doc.clone().get_heads()),
            heads,
            "mode choice is view state"
        );
    }

    fn toolbar_keyboard_tab_and_enter_follow_the_painted_control_order() {
        use ui::headless::ControlInput;
        let mut app = sample_app();
        let controls = header_controls(&mut app);
        let diff = controls
            .iter()
            .find(|control| control.name.as_deref() == Some("Diff"))
            .unwrap();
        app.harness()
            .operate_control(&diff.id, &ControlInput::Focus)
            .unwrap();
        app.harness().key("ArrowRight");
        app.settle();
        assert_eq!(
            app.with_surface::<DiffMergeApp, _>(|view| view.mode),
            Some(Mode::Merge)
        );
        for name in [
            "Previous change",
            "Next change",
            "Load sample",
            "Open files",
        ] {
            app.harness().tab(false);
            assert!(
                app.harness()
                    .controls()
                    .iter()
                    .any(|control| control.focused && control.name.as_deref() == Some(name)),
                "Tab reaches {name} in painted order"
            );
        }
        let diff = header_controls(&mut app)
            .into_iter()
            .find(|control| control.name.as_deref() == Some("Diff"))
            .unwrap();
        app.harness()
            .operate_control(&diff.id, &ControlInput::Focus)
            .unwrap();
        app.harness().key("ArrowLeft");
        app.harness().key("Enter");
        app.settle();
        assert_eq!(
            app.with_surface::<DiffMergeApp, _>(|view| view.mode),
            Some(Mode::Diff)
        );
    }

    fn phone_diff_uses_an_accessible_one_source_switcher() {
        let (mut app, state) = demo();
        app.set_mode_for_test(Mode::Diff);
        let mut app = merge_frame(app, state, (390.0, 760.0));
        let (app, state) = Framed::split(&mut app);
        let diff = Widget::accessibility(&app.diff).expect("active diff child is accessible");
        assert!(
            diff.name
                .as_deref()
                .is_some_and(|name| name.starts_with("Diff between"))
        );
        let root = Widget::accessibility(&*app)
            .expect("source controls are published by their actual action owner");
        assert_eq!(
            root.children
                .iter()
                .filter(|child| child
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Show ")))
                .count(),
            2
        );
        assert!(Widget::perform_semantic_action(
            &mut *app,
            "diffmerge.diff-source:right",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut state.doc),
            &state.root,
        ));
        assert!(app.diff.compact_showing_right());
    }

    #[test]
    fn phone_diff_explains_opposite_only_lines_without_changing_the_comparison() {
        use crate::actions::{DiffmergeAction, DiffmergeEffect};
        let mut app = TestApp::<DiffmergeShell>::new().unwrap();
        app.invoke(
            ui::tilo_resource::Invoke::apply(&DiffmergeAction::OpenComparison {
                left_label: "before.txt".into(),
                left: "head\ntail\n".into(),
                right_label: "after.txt".into(),
                right: "head\none\ntwo\nthree\ntail\n".into(),
            })
            .unwrap(),
        )
        .unwrap();
        app.effect(&DiffmergeEffect::ShowDiff).unwrap();
        app.harness().resize((390, 760)).unwrap();
        app.settle();
        let comparison = app.state().comparison;
        let heads = app.peer_document().get_heads();
        let expected =
            crate::i18n::t_args("diff-gap-added-right", &[("count", crate::i18n::count(3))]);
        assert!(
            app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some(expected.as_str())),
            "the actual exported narrow source explains its three alignment rows"
        );
        let right = app
            .with_surface::<DiffMergeApp, _>(|surface| {
                let controls = surface.diff.access_controls();
                let gap = controls
                    .iter()
                    .find(|node| node.name.as_deref() == Some(expected.as_str()))
                    .unwrap();
                let bounds = gap.bounds.unwrap().to_rect();
                assert!(
                    bounds.area() > 0.0
                        && bounds.x0 >= 0.0
                        && bounds.x1 <= 390.0
                        && bounds.y0 >= 0.0
                        && bounds.y1 <= 760.0
                );
                let source = controls
                    .iter()
                    .find(|node| {
                        node.action_target.as_deref() == Some("diffmerge.diff-source:right")
                    })
                    .unwrap();
                let bounds = source.bounds.unwrap().to_rect();
                bounds.center()
            })
            .unwrap();
        app.harness().click_at(right.x, right.y);
        app.settle();
        assert!(
            app.with_surface::<DiffMergeApp, _>(|surface| surface.diff.compact_showing_right())
                .unwrap()
        );
        assert!(
            !app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some(expected.as_str())),
            "the opposite source contains the actual added lines rather than filler"
        );
        assert_eq!(app.state().comparison, comparison);
        assert_eq!(
            app.peer_document().get_heads(),
            heads,
            "explaining and switching aligned sources is only a local view"
        );
        assert!(app.harness().native_app_audit().is_certified());
    }

    #[test]
    fn stale_diff_block_operation_refuses_a_recomputed_block_at_the_same_index() {
        let mut app = DiffMergeApp::new();
        app.diff.set_inputs(
            "before.txt",
            "same\nold value\n",
            "after.txt",
            "same\nnew value\n",
        );
        let stale_fingerprint = app.diff.block_fingerprint(0).expect("one change block");
        app.diff.set_inputs(
            "before.txt",
            "same\nold value\n",
            "after.txt",
            "same\nreplacement value\n",
        );
        let error = app
            .answer_view(DiffmergeEffect::Show {
                block: 0,
                fingerprint: stale_fingerprint,
            })
            .expect_err("stale local operation must explain why its target is unsafe");
        assert!(error.contains("comparison changed"));
    }

    fn narrow_status_bubble_stays_within_the_viewport_for_long_errors() {
        let (app, state) = staged(Some("error"));
        let app = merge_frame(app, state, (390.0, 760.0));
        let error_area = app.content_rect();
        assert!(
            error_area.x0 >= app.rect.x0 && error_area.x1 <= app.rect.x1,
            "error surface {error_area:?} in {:?}",
            app.rect
        );
        assert!(
            error_area.y0 >= app.rect.y0 && error_area.y1 <= app.rect.y1,
            "error surface {error_area:?} in {:?}",
            app.rect
        );
        let semantics = app
            .accessibility()
            .expect("the visible error remains accessible");
        assert!(
            semantics
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some("File loading failed")
                    && node.description.as_deref() == app.load_error.as_deref()),
            "the shared error surface retains the full reason"
        );
    }

    /// A chip's accessibility action resolves its hunk through the same
    /// path a click takes.
    #[test]
    fn a_chip_accessibility_action_resolves_its_hunk() {
        let (app, state) = demo();
        let mut app = merge_frame(app, state, (980.0, 720.0));
        let (app, state) = Framed::split(&mut app);
        let merge = Widget::accessibility(&app.merge)
            .expect("active merge content is exposed by the retained child");
        assert_eq!(merge.name.as_deref(), Some("Three-way merge"));
        let target = merge
            .children
            .iter()
            .filter_map(|child| child.action_target.clone())
            .find(|target| target.ends_with(":both"))
            .expect("a drawn resolution action is accessible");
        let hunk: usize = target
            .trim_start_matches("diffmerge.merge.resolve:")
            .trim_end_matches(":both")
            .parse()
            .unwrap();
        assert!(Widget::perform_semantic_action(
            &mut *app,
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut state.doc),
            &state.root,
        ));
        assert_eq!(
            app.merge.model().hunks[hunk].resolution,
            crate::model::Resolution::TookBoth
        );
    }

    fn root_accessibility_explains_empty_and_error_content() {
        let (empty, state) = staged(Some("empty"));
        let empty = merge_frame(empty, state, (980.0, 720.0));
        let empty_semantics = Widget::accessibility(&*empty).expect("empty semantics");
        let names = empty_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"No files loaded"), "{names:?}");

        let (error, state) = staged(Some("error"));
        let error = merge_frame(error, state, (980.0, 720.0));
        let error_semantics = Widget::accessibility(&*error).expect("error semantics");
        let names = error_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"File loading failed"), "{names:?}");
    }
}
