//! Root widget for the git client.
//!
//! Owns the in-memory [`RepoModel`], the currently-shown diff, the active
//! screen and all transient selection / scroll state. Durable preferences
//! live in the CRDT via [`Settings`]. The widget draws everything itself
//! into the Vello scene — a top app bar with a screen switcher, then the
//! active screen (changes / history / panels). Events are routed by
//! hit-testing the regions each screen emits.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::changes::{ChangesColumn, ChangesHit};
use crate::diffview::{self, DiffView};
use crate::draw;
use crate::graph::{self, GraphLayout};
use crate::history::HistoryView;
use crate::model::{ChangeArea, FileChange, FileDiff, RepoModel};
use crate::panels::{PanelsView, RemoteAction};
use crate::settings::{DiffLayout, Settings};

/// Which top-level screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Changes,
    History,
    Panels,
}

const TABS: [(Screen, &str); 3] = [
    (Screen::Changes, "Changes"),
    (Screen::History, "History"),
    (Screen::Panels, "Branches & Remotes"),
];

const APPBAR_H: f64 = 56.0;
const CHANGES_COL_W: f64 = 320.0;

pub struct GitClientApp {
    model: RepoModel,
    diffs: HashMap<String, FileDiff>,
    graph_layout: GraphLayout,

    settings: Settings,
    /// Set once after the first sync seeds the settings into the doc.
    settings_seeded: bool,
    /// Raised when a local setting toggle must be flushed back to the doc.
    settings_dirty: bool,
    /// Cached hit rects for the app-bar settings controls, set during draw.
    settings_seg: Rect,
    settings_syn: Rect,

    screen: Screen,
    rect: Rect,

    // --- Changes screen state ---
    selected_change: Option<(ChangeArea, usize)>,
    changes_scroll: f64,
    diff_scroll: f64,
    commit_message: String,
    msg_focused: bool,
    hover_hunk: Option<usize>,

    // --- History screen state ---
    selected_commit: usize,
    history_scroll: f64,
    detail_scroll: f64,

    // --- Panels screen state ---
    hover_action: Option<RemoteAction>,

    last_mouse: Point,

    // Click regions emitted by the last draw, used for hit-testing the
    // next click. Boxed closures keep geometry out of the event handler.
    change_hits: Vec<Box<dyn Fn(Point) -> Option<ChangesHit>>>,
    diff_hunk_buttons: Vec<diffview::HunkButton>,
    history_rows: Vec<Rect>,
    panel_hits: Vec<Box<dyn Fn(Point) -> Option<RemoteAction>>>,
    tab_rects: Vec<(Screen, Rect)>,
}

impl GitClientApp {
    /// Build the app, loading the repository discovered from the current
    /// directory (or `GIT_CLIENT_REPO`). Falls back to the bundled sample
    /// when no repository is reachable.
    ///
    /// Setting `GIT_CLIENT_SAMPLE=1` forces the bundled fixture regardless
    /// of any reachable repository — the headless screenshot path uses
    /// this so the capture is a fully populated, deterministic state
    /// (rename + delete + new file across all three areas, a multi-lane
    /// commit graph) rather than whatever happens to sit around the CWD.
    pub fn new() -> Self {
        let force_sample = std::env::var("GIT_CLIENT_SAMPLE")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false);
        let (model, diffs) = if force_sample {
            crate::sample::model()
        } else {
            Self::load_repo().unwrap_or_else(crate::sample::model)
        };
        Self::from_model(model, diffs)
    }

    /// Construct from an explicit model — used by the sample path and tests.
    pub fn from_model(mut model: RepoModel, diffs: HashMap<String, FileDiff>) -> Self {
        let graph_layout = graph::layout(&mut model.commits);
        // Pre-select the first changed file so the diff pane is populated.
        let selected_change = first_change(&model);
        // Allow the headless capture to open a specific screen so each
        // screen can be reviewed; defaults to Changes for the live app.
        let screen = match std::env::var("GIT_CLIENT_SCREEN").as_deref() {
            Ok("history") => Screen::History,
            Ok("panels") => Screen::Panels,
            _ => Screen::Changes,
        };
        let mut settings = Settings::default();
        let split_forced = std::env::var("GIT_CLIENT_SPLIT").is_ok();
        if split_forced {
            settings.diff_layout = DiffLayout::SideBySide;
        }
        Self {
            model,
            diffs,
            graph_layout,
            settings,
            settings_seeded: false,
            settings_dirty: split_forced,
            settings_seg: Rect::ZERO,
            settings_syn: Rect::ZERO,
            screen,
            rect: Rect::ZERO,
            selected_change,
            changes_scroll: 0.0,
            diff_scroll: 0.0,
            commit_message: String::new(),
            msg_focused: false,
            hover_hunk: None,
            selected_commit: 0,
            history_scroll: 0.0,
            detail_scroll: 0.0,
            hover_action: None,
            last_mouse: Point::ZERO,
            change_hits: Vec::new(),
            diff_hunk_buttons: Vec::new(),
            history_rows: Vec::new(),
            panel_hits: Vec::new(),
            tab_rects: Vec::new(),
        }
    }

    fn load_repo() -> Option<(RepoModel, HashMap<String, FileDiff>)> {
        let path = std::env::var("GIT_CLIENT_REPO")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        crate::repo::load(&path)
    }

    /// Resolve the [`FileDiff`] for the currently selected change, if any.
    fn current_diff(&self) -> Option<&FileDiff> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        let path = &list.get(idx)?.path;
        self.diffs.get(path)
    }

    fn selected_is_staged(&self) -> bool {
        matches!(self.selected_change, Some((ChangeArea::Staged, _)))
    }

    // --- Mutations on the model (mirror a real git op) -----------------

    /// Move a file between staged and the worktree. In the live app this
    /// would call into git2; here it updates the model so the UI reflects
    /// the move immediately (the backend refresh would re-derive it).
    fn toggle_stage(&mut self, area: ChangeArea, idx: usize) {
        match area {
            ChangeArea::Staged => {
                if idx < self.model.staged.len() {
                    let mut f = self.model.staged.remove(idx);
                    f.area = ChangeArea::Unstaged;
                    self.model.unstaged.push(f);
                }
            }
            ChangeArea::Unstaged => {
                if idx < self.model.unstaged.len() {
                    let mut f = self.model.unstaged.remove(idx);
                    f.area = ChangeArea::Staged;
                    self.model.staged.push(f);
                }
            }
            ChangeArea::Untracked => {
                if idx < self.model.untracked.len() {
                    let mut f = self.model.untracked.remove(idx);
                    f.area = ChangeArea::Staged;
                    f.kind = crate::model::ChangeKind::Added;
                    self.model.staged.push(f);
                }
            }
        }
        self.selected_change = first_change(&self.model);
    }

    fn stage_all(&mut self) {
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.unstaged.drain(..) {
            f.area = ChangeArea::Staged;
            moved.push(f);
        }
        for mut f in self.model.untracked.drain(..) {
            f.area = ChangeArea::Staged;
            f.kind = crate::model::ChangeKind::Added;
            moved.push(f);
        }
        self.model.staged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    fn unstage_all(&mut self) {
        let mut moved: Vec<FileChange> = Vec::new();
        for mut f in self.model.staged.drain(..) {
            f.area = ChangeArea::Unstaged;
            moved.push(f);
        }
        self.model.unstaged.append(&mut moved);
        self.selected_change = first_change(&self.model);
    }

    fn do_commit(&mut self) {
        if self.model.staged.is_empty() || self.commit_message.trim().is_empty() {
            return;
        }
        // Synthesise a new commit at the tip so the history view reflects
        // the action; a real backend would write the object and re-read.
        let staged = std::mem::take(&mut self.model.staged);
        let parent = self.model.commits.first().map(|c| c.id.clone());
        let id = format!("{:07x}", self.model.commits.len() * 0x1f7 + 0xa10);
        let summary = self.commit_message.lines().next().unwrap_or("").to_string();
        let body = self
            .commit_message
            .splitn(2, '\n')
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let new = crate::model::Commit {
            id: id.clone(),
            summary,
            body,
            author_name: self.model.author_name.clone(),
            author_email: self.model.author_email.clone(),
            when: "just now".into(),
            time: chrono::Utc::now().timestamp(),
            parents: parent.into_iter().collect(),
            refs: vec![
                crate::model::RefLabel { name: "HEAD".into(), kind: crate::model::RefKind::Head },
                crate::model::RefLabel { name: self.model.head.clone(), kind: crate::model::RefKind::LocalBranch },
            ],
            lane: 0,
            files: staged,
        };
        // Drop the old HEAD/branch decoration on the previous tip.
        if let Some(prev) = self.model.commits.first_mut() {
            prev.refs.retain(|r| {
                r.kind != crate::model::RefKind::Head && r.name != self.model.head
            });
        }
        self.model.commits.insert(0, new);
        self.graph_layout = graph::layout(&mut self.model.commits);
        self.model.graph_width = self.graph_layout.lanes;
        self.commit_message.clear();
        self.msg_focused = false;
        self.selected_change = first_change(&self.model);
    }

    // --- Drawing -------------------------------------------------------

    fn draw_appbar(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + APPBAR_H);
        draw::fill_rect(scene, t, bar, c.surfaces.raised);
        draw::hline(scene, t, bar.y1, bar.x0, bar.x1, c.border_standard);

        // Repo name + branch.
        draw::text_weighted(
            scene, tr, t, &self.model.name, 15.0, c.text_primary, bar.x0 + 20.0, bar.y0 + 9.0, None,
            TextWeight::Bold,
        );
        let nw = tr.measure_weighted(&self.model.name, 15.0, None, TextWeight::Bold).0 as f64;
        draw::pill(
            scene, tr, t, bar.x0 + 28.0 + nw, bar.y0 + 9.0, 20.0,
            &self.model.head, Color::WHITE, theme.accent_primary, None,
        );
        // change count subtitle
        let sub = format!("{} pending change{}", self.model.change_count(),
            if self.model.change_count() == 1 { "" } else { "s" });
        draw::text(scene, tr, t, &sub, 11.5, c.text_dim, bar.x0 + 20.0, bar.y0 + 30.0, None);

        // Screen tabs (centered group on the right side of name).
        self.tab_rects.clear();
        let tab_h = 32.0;
        let tab_y = bar.y0 + (APPBAR_H - tab_h) / 2.0;
        // Measure total width and right-align the tab group, leaving room
        // for the settings cluster.
        let mut widths: Vec<f64> = Vec::new();
        for (_, label) in TABS.iter() {
            let w = tr.measure_weighted(label, 13.0, None, TextWeight::Medium).0 as f64 + 28.0;
            widths.push(w);
        }
        let settings_w = 220.0;
        let total: f64 = widths.iter().sum::<f64>() + (TABS.len() as f64 - 1.0) * 6.0;
        let mut tx = bar.x1 - settings_w - total - 20.0;
        let group_bg = Rect::new(tx - 6.0, tab_y, tx + total + 6.0, tab_y + tab_h);
        draw::fill_round(scene, t, group_bg, 9.0, c.surfaces.base);
        for (i, (screen, label)) in TABS.iter().enumerate() {
            let w = widths[i];
            let trect = Rect::new(tx, tab_y, tx + w, tab_y + tab_h);
            let active = *screen == self.screen;
            if active {
                draw::fill_round(scene, t, trect, 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { c.text_dim };
            let lw = tr.measure_weighted(label, 13.0, None, TextWeight::Medium).0 as f64;
            draw::text_weighted(
                scene, tr, t, label, 13.0, fg, tx + (w - lw) / 2.0, tab_y + 8.0, None,
                TextWeight::Medium,
            );
            self.tab_rects.push((*screen, trect));
            tx += w + 6.0;
        }

        // Settings cluster on the far right: diff layout toggle + flags.
        self.draw_settings_cluster(scene, tr, t, theme, bar, settings_w);
    }

    fn draw_settings_cluster(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        bar: Rect,
        cluster_w: f64,
    ) {
        let c = &theme.tokens.colors;
        let x = bar.x1 - cluster_w;
        let y = bar.y0 + (APPBAR_H - 32.0) / 2.0;
        // Diff-layout segmented control.
        let seg_w = 150.0;
        let seg = Rect::new(x, y, x + seg_w, y + 32.0);
        draw::fill_round(scene, t, seg, 9.0, c.surfaces.base);
        let half = seg_w / 2.0;
        let unified_active = self.settings.diff_layout == DiffLayout::Unified;
        let active_rect = if unified_active {
            Rect::new(seg.x0 + 3.0, seg.y0 + 3.0, seg.x0 + half, seg.y1 - 3.0)
        } else {
            Rect::new(seg.x0 + half, seg.y0 + 3.0, seg.x1 - 3.0, seg.y1 - 3.0)
        };
        draw::fill_round(scene, t, active_rect, 7.0, theme.accent_primary);
        for (i, label) in ["Unified", "Split"].iter().enumerate() {
            let active = (i == 0) == unified_active;
            let fg = if active { Color::WHITE } else { c.text_dim };
            let cx = seg.x0 + half * i as f64;
            let lw = tr.measure_weighted(label, 12.0, None, TextWeight::Medium).0 as f64;
            draw::text_weighted(
                scene, tr, t, label, 12.0, fg, cx + (half - lw) / 2.0, y + 8.0, None,
                TextWeight::Medium,
            );
        }
        // Note: the segmented control's click rect is stored as the whole
        // seg; the click handler flips the layout.
        self.settings_seg = seg;

        // Syntax-highlight pill toggle.
        let hx = seg.x1 + 10.0;
        let hl = Rect::new(hx, y, hx + 50.0, y + 32.0);
        let on = self.settings.syntax_highlight;
        draw::fill_round(scene, t, hl, 9.0, if on { theme.accent_primary } else { c.surfaces.base });
        let hlfg = if on { Color::WHITE } else { c.text_dim };
        draw::text_weighted(scene, tr, t, "Syn", 12.0, hlfg, hx + 13.0, y + 8.0, None, TextWeight::Medium);
        self.settings_syn = hl;
    }

    fn draw_changes(&mut self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, body: Rect) {
        let col = Rect::new(body.x0, body.y0, body.x0 + CHANGES_COL_W, body.y1);
        self.change_hits = ChangesColumn::draw(
            scene, tr, t, theme, col, &self.model, self.selected_change, self.changes_scroll,
            &self.commit_message, self.msg_focused,
        );

        // Diff pane fills the rest.
        let pane = Rect::new(col.x1, body.y0, body.x1, body.y1);
        // Pane header: file path + +/- summary.
        let header = Rect::new(pane.x0, pane.y0, pane.x1, pane.y0 + 36.0);
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, header, c.surfaces.raised);
        draw::hline(scene, t, header.y1, header.x0, header.x1, c.border_standard);
        if let Some(diff) = self.current_diff() {
            let title = if let Some(old) = &diff.old_path {
                format!("{}  →  {}", old, diff.path)
            } else {
                diff.path.clone()
            };
            draw::text_weighted(
                scene, tr, t, &title, 13.0, c.text_primary, header.x0 + 16.0, header.y0 + 9.0,
                Some((header.width() - 160.0) as f32), TextWeight::Medium,
            );
            let summary = format!("+{}  −{}", diff.additions(), diff.deletions());
            let aw = draw::mono_width(tr, &summary, 12.0) as f64;
            draw::mono(scene, tr, t, &summary, 12.0, c.text_dim, header.x1 - aw - 16.0, header.y0 + 10.0);
        } else {
            draw::text(scene, tr, t, "Select a file to view its diff", 13.0, c.text_dim, header.x0 + 16.0, header.y0 + 9.0, None);
        }

        let viewport = Rect::new(pane.x0, header.y1, pane.x1, pane.y1);
        let staged = self.selected_is_staged();
        if let Some(diff) = self.diffs.get(self.current_path().as_deref().unwrap_or("")) {
            self.diff_hunk_buttons = DiffView::draw(
                scene, tr, t, theme, viewport, diff, self.diff_scroll, &self.settings,
                self.hover_hunk, staged,
            );
        } else {
            draw::fill_rect(scene, t, viewport, c.surfaces.base);
            self.diff_hunk_buttons.clear();
        }
    }

    fn current_path(&self) -> Option<String> {
        let (area, idx) = self.selected_change?;
        let list = match area {
            ChangeArea::Staged => &self.model.staged,
            ChangeArea::Unstaged => &self.model.unstaged,
            ChangeArea::Untracked => &self.model.untracked,
        };
        list.get(idx).map(|f| f.path.clone())
    }

    fn draw_history(&mut self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, body: Rect) {
        let split = body.x0 + (body.width() * 0.56).max(420.0).min(body.width() - 320.0);
        let list = Rect::new(body.x0, body.y0, split, body.y1);
        self.history_rows = HistoryView::draw_list(
            scene, tr, t, theme, list, &self.model, &self.graph_layout, self.selected_commit,
            self.history_scroll,
        );
        let detail = Rect::new(split, body.y0, body.x1, body.y1);
        if let Some(commit) = self.model.commits.get(self.selected_commit) {
            HistoryView::draw_detail(scene, tr, t, theme, detail, commit, self.detail_scroll);
        }
    }

    fn draw_panels(&mut self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, body: Rect) {
        self.panel_hits = PanelsView::draw(scene, tr, t, theme, body, &self.model, self.hover_action);
    }

    // --- Event helpers -------------------------------------------------

    fn handle_click(&mut self, pos: Point) {
        // Tabs.
        for (screen, rect) in &self.tab_rects {
            if rect.contains(pos) {
                self.screen = *screen;
                self.msg_focused = false;
                return;
            }
        }
        // Settings segmented control + syntax toggle.
        if self.settings_seg.contains(pos) {
            self.settings.toggle_layout();
            self.diff_scroll = 0.0;
            self.settings_dirty = true;
            return;
        }
        if self.settings_syn.contains(pos) {
            self.settings.syntax_highlight = !self.settings.syntax_highlight;
            self.settings_dirty = true;
            return;
        }

        match self.screen {
            Screen::Changes => self.handle_changes_click(pos),
            Screen::History => self.handle_history_click(pos),
            Screen::Panels => self.handle_panels_click(pos),
        }
    }

    fn handle_changes_click(&mut self, pos: Point) {
        // Hunk-stage buttons take priority (they overlap the diff body).
        if let Some(hunk) = diffview::hunk_button_at(&self.diff_hunk_buttons, pos) {
            self.stage_hunk(hunk);
            return;
        }
        // Resolve against the changes column regions.
        let mut hit: Option<ChangesHit> = None;
        for f in &self.change_hits {
            if let Some(h) = f(pos) {
                hit = Some(h);
                break;
            }
        }
        let Some(hit) = hit else {
            self.msg_focused = false;
            return;
        };
        match hit {
            ChangesHit::SelectFile(area, idx) => {
                self.selected_change = Some((area, idx));
                self.diff_scroll = 0.0;
                self.msg_focused = false;
            }
            ChangesHit::ToggleStage(area, idx) => self.toggle_stage(area, idx),
            ChangesHit::StageAll => self.stage_all(),
            ChangesHit::UnstageAll => self.unstage_all(),
            ChangesHit::Commit => self.do_commit(),
            ChangesHit::FocusMessage => self.msg_focused = true,
        }
    }

    /// Stage (or unstage) a single hunk. Mirrors `git add -p`; here we move
    /// the whole file once any hunk is staged, which keeps the model
    /// consistent for the demo while still proving the per-hunk hit-test.
    fn stage_hunk(&mut self, _hunk: usize) {
        if let Some((area, idx)) = self.selected_change {
            self.toggle_stage(area, idx);
        }
    }

    fn handle_history_click(&mut self, pos: Point) {
        for (i, rect) in self.history_rows.iter().enumerate() {
            if rect.contains(pos) {
                self.selected_commit = i;
                self.detail_scroll = 0.0;
                return;
            }
        }
    }

    fn handle_panels_click(&mut self, pos: Point) {
        for f in &self.panel_hits {
            if let Some(action) = f(pos) {
                self.apply_remote_action(action);
                return;
            }
        }
    }

    /// Apply a remote action against the head branch's tracking state.
    /// The real backend would invoke fetch/pull/push; we reconcile the
    /// ahead/behind counters so the UI shows the effect.
    fn apply_remote_action(&mut self, action: RemoteAction) {
        if let Some(b) = self.model.branches.iter_mut().find(|b| b.is_head) {
            match action {
                RemoteAction::Fetch => { /* refresh remote refs; counters unchanged */ }
                RemoteAction::Pull => {
                    b.behind = 0;
                }
                RemoteAction::Push => {
                    b.ahead = 0;
                }
            }
        }
    }

    fn scroll_active(&mut self, dy: f64, pos: Point) {
        match self.screen {
            Screen::Changes => {
                if pos.x < self.rect.x0 + CHANGES_COL_W {
                    self.changes_scroll = (self.changes_scroll - dy).max(0.0);
                } else {
                    let max = self
                        .current_diff()
                        .map(DiffView::content_height)
                        .unwrap_or(0.0)
                        - (self.rect.height() - APPBAR_H - 36.0);
                    self.diff_scroll = (self.diff_scroll - dy).clamp(0.0, max.max(0.0));
                }
            }
            Screen::History => {
                let split = self.rect.x0 + (self.rect.width() * 0.56).max(420.0);
                if pos.x < split {
                    let max = HistoryView::content_height(&self.model) - (self.rect.height() - APPBAR_H);
                    self.history_scroll = (self.history_scroll - dy).clamp(0.0, max.max(0.0));
                } else {
                    self.detail_scroll = (self.detail_scroll - dy).max(0.0);
                }
            }
            Screen::Panels => {}
        }
    }
}

fn first_change(model: &RepoModel) -> Option<(ChangeArea, usize)> {
    if !model.staged.is_empty() {
        Some((ChangeArea::Staged, 0))
    } else if !model.unstaged.is_empty() {
        Some((ChangeArea::Unstaged, 0))
    } else if !model.untracked.is_empty() {
        Some((ChangeArea::Untracked, 0))
    } else {
        None
    }
}

impl Widget for GitClientApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.settings_seeded {
            // First sync: adopt any persisted settings, then write ours
            // back so the doc is the source of truth henceforth. If a
            // local toggle was already pending (e.g. forced at startup),
            // our value wins over an empty doc.
            if !self.settings_dirty {
                self.settings = Settings::read(state);
            }
            self.settings.write(state);
            self.settings_seeded = true;
            self.settings_dirty = false;
        } else if self.settings_dirty {
            self.settings.write(state);
            self.settings_dirty = false;
        } else {
            // Pick up remote changes to settings (collaboration).
            self.settings = Settings::read(state);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app draws itself; a single full-size leaf node carries it.
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, transform, self.rect, c.surfaces.base);

        self.draw_appbar(scene, tr, transform, theme);

        let body = Rect::new(self.rect.x0, self.rect.y0 + APPBAR_H, self.rect.x1, self.rect.y1);
        match self.screen {
            Screen::Changes => self.draw_changes(scene, tr, transform, theme, body),
            Screen::History => self.draw_history(scene, tr, transform, theme, body),
            Screen::Panels => self.draw_panels(scene, tr, transform, theme, body),
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Hover state for hunk buttons + remote actions.
                self.hover_hunk = diffview::hunk_button_at(&self.diff_hunk_buttons, self.last_mouse);
                self.hover_action = None;
                for f in &self.panel_hits {
                    if let Some(a) = f(self.last_mouse) {
                        self.hover_action = Some(a);
                        break;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.handle_click(self.last_mouse);
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 36.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll_active(dy, self.last_mouse);
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                self.handle_key(key_event, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "GitClientApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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

impl GitClientApp {
    fn handle_key(&mut self, ev: &KeyEvent, handled: &mut bool) {
        if ev.state != ElementState::Pressed {
            return;
        }
        // Commit-message editing takes keys when focused.
        if self.msg_focused {
            match &ev.logical_key {
                Key::Named(NamedKey::Backspace) => {
                    self.commit_message.pop();
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    // Enter commits; Shift+Enter would add a newline, but
                    // we keep it simple and commit on Enter.
                    self.do_commit();
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.msg_focused = false;
                    *handled = true;
                }
                Key::Named(NamedKey::Space) => {
                    self.commit_message.push(' ');
                    *handled = true;
                }
                Key::Character(s) => {
                    self.commit_message.push_str(s);
                    *handled = true;
                }
                _ => {}
            }
            return;
        }

        // Global shortcuts: 1/2/3 switch screens.
        if let Key::Character(s) = &ev.logical_key {
            match s.as_str() {
                "1" => { self.screen = Screen::Changes; *handled = true; }
                "2" => { self.screen = Screen::History; *handled = true; }
                "3" => { self.screen = Screen::Panels; *handled = true; }
                "d" => {
                    self.settings.toggle_layout();
                    self.settings_dirty = true;
                    *handled = true;
                }
                _ => {}
            }
        }

        // Arrow navigation in the active list.
        match (&ev.logical_key, self.screen) {
            (Key::Named(NamedKey::ArrowDown), Screen::History) => {
                if self.selected_commit + 1 < self.model.commits.len() {
                    self.selected_commit += 1;
                    self.detail_scroll = 0.0;
                }
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowUp), Screen::History) => {
                self.selected_commit = self.selected_commit.saturating_sub(1);
                self.detail_scroll = 0.0;
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowDown), Screen::Changes) => {
                self.select_next_change(1);
                *handled = true;
            }
            (Key::Named(NamedKey::ArrowUp), Screen::Changes) => {
                self.select_next_change(-1);
                *handled = true;
            }
            _ => {}
        }
    }

    fn select_next_change(&mut self, dir: i32) {
        let all = self.model.all_changes().len();
        if all == 0 {
            return;
        }
        // Flatten current selection to a global index.
        let cur = match self.selected_change {
            Some((ChangeArea::Staged, i)) => i,
            Some((ChangeArea::Unstaged, i)) => self.model.staged.len() + i,
            Some((ChangeArea::Untracked, i)) => self.model.staged.len() + self.model.unstaged.len() + i,
            None => 0,
        } as i32;
        let next = (cur + dir).clamp(0, all as i32 - 1) as usize;
        // Map back to (area, idx).
        let s = self.model.staged.len();
        let u = self.model.unstaged.len();
        self.selected_change = Some(if next < s {
            (ChangeArea::Staged, next)
        } else if next < s + u {
            (ChangeArea::Unstaged, next - s)
        } else {
            (ChangeArea::Untracked, next - s - u)
        });
        self.diff_scroll = 0.0;
    }
}
