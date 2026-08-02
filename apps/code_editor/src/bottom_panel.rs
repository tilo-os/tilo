//! The collapsible bottom panel: a tabbed dock for Problems and Terminal.
//! Problems lists diagnostics (a lightweight, real linter over the active
//! buffer — see `app.rs`); Terminal hosts a live embedded shell, spawned on
//! first open, and shows an honest empty-state note until then (never a
//! fabricated build log).
//!
//! Real LSP diagnostics are deferred — see the TODOs in `app.rs`. The panel
//! renders the data the app feeds it.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

pub const PANEL_HEIGHT: f64 = 160.0;
const TAB_H: f64 = 30.0;
const ROW_H: f64 = 20.0;

/// Which dock tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelTab {
    Problems,
    Terminal,
}

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A hard error (LSP severity 1). Rendered in the danger colour.
    Error,
    Warning,
    Info,
    Hint,
}

/// One diagnostic row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

pub struct BottomPanel {
    rect: Rect,
    tab: PanelTab,
    problems: Vec<Diagnostic>,
    tab_rects: Vec<(f64, f64, PanelTab)>,
    last_cursor: Point,
    /// Set true when the user clicks a tab — read+cleared by the app.
    tab_changed: bool,
    /// When true, the Terminal tab's body is owned by an embedded live
    /// `TerminalWidget` the app draws over this panel's content rect, so the
    /// panel leaves that region blank instead of rendering the static fallback.
    has_live_terminal: bool,
    /// Note shown in the Terminal body when no live terminal is present (the
    /// pre-spawn invitation, or "shell unavailable" on a spawn failure).
    terminal_note: Option<String>,
    dirty: bool,
}

impl BottomPanel {
    pub fn new() -> Self {
        BottomPanel {
            rect: Rect::ZERO,
            tab: PanelTab::Problems,
            problems: Vec::new(),
            tab_rects: Vec::new(),
            last_cursor: Point::ZERO,
            tab_changed: false,
            has_live_terminal: false,
            terminal_note: None,
            dirty: true,
        }
    }

    pub fn set_problems(&mut self, problems: Vec<Diagnostic>) {
        if self.problems != problems {
            self.problems = problems;
            self.dirty = true;
        }
    }

    /// Mark whether a live embedded terminal owns the Terminal tab body.
    pub fn set_has_live_terminal(&mut self, live: bool) {
        if self.has_live_terminal != live {
            self.has_live_terminal = live;
            self.dirty = true;
        }
    }

    /// Set (or clear) the note shown when there is no live terminal.
    pub fn set_terminal_note(&mut self, note: Option<String>) {
        if self.terminal_note != note {
            self.terminal_note = note;
            self.dirty = true;
        }
    }

    /// The currently-visible dock tab.
    pub fn active_tab(&self) -> PanelTab {
        self.tab
    }

    /// The content region below the tab strip — where the embedded terminal is
    /// placed and drawn.
    pub fn content_rect(&self) -> Rect {
        Rect::new(self.rect.x0, self.rect.y0 + TAB_H, self.rect.x1, self.rect.y1)
    }

    pub fn problem_count(&self) -> usize {
        self.problems.len()
    }

    pub fn set_tab(&mut self, tab: PanelTab) {
        if self.tab != tab {
            self.tab = tab;
            self.dirty = true;
        }
    }

    pub fn take_tab_changed(&mut self) -> bool {
        std::mem::take(&mut self.tab_changed)
    }
}

impl Widget for BottomPanel {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(PANEL_HEIGHT as f32),
                },
                flex_shrink: 0.0,
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
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &self.rect);
        // Top divider.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y0),
                Point::new(self.rect.x1, self.rect.y0),
            ),
        );

        // Tab strip.
        self.tab_rects.clear();
        let labels = [
            (PanelTab::Problems, format!("Problems ({})", self.problems.len())),
            (PanelTab::Terminal, "Terminal".to_string()),
        ];
        let mut x = self.rect.x0 + 8.0;
        for (tab, label) in labels.iter() {
            let (w, _) = text_renderer.measure(label, 12.0, None);
            let x1 = x + w as f64 + 24.0;
            self.tab_rects.push((x, x1, *tab));
            let active = self.tab == *tab;
            let color = if active { c.text_primary } else { c.text_dim };
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                color,
                transform * Affine::translate((x + 6.0, self.rect.y0 + 9.0)),
                None,
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
            if active {
                let underline = Rect::new(x, self.rect.y0 + TAB_H - 2.0, x1, self.rect.y0 + TAB_H);
                scene.fill(Fill::NonZero, transform, c.accent[5], None, &underline);
            }
            x = x1;
        }
        // Tab strip baseline.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y0 + TAB_H),
                Point::new(self.rect.x1, self.rect.y0 + TAB_H),
            ),
        );

        let content_y0 = self.rect.y0 + TAB_H + 6.0;
        match self.tab {
            PanelTab::Problems => {
                if self.problems.is_empty() {
                    text_renderer.draw_text(
                        scene,
                        "No problems detected in the workspace.",
                        13.0,
                        c.text_dim,
                        transform * Affine::translate((self.rect.x0 + 14.0, content_y0)),
                        None,
                    );
                } else {
                    for (i, d) in self.problems.iter().enumerate() {
                        let y = content_y0 + i as f64 * ROW_H;
                        if y > self.rect.y1 - ROW_H {
                            break;
                        }
                        let (glyph, sev_color) = match d.severity {
                            Severity::Error => ("\u{2716}", c.semantic.danger),
                            Severity::Warning => ("\u{26A0}", c.semantic.warning),
                            Severity::Info => ("\u{2139}", c.semantic.info),
                            Severity::Hint => ("\u{2022}", c.text_dim),
                        };
                        text_renderer.draw_text(
                            scene,
                            glyph,
                            13.0,
                            sev_color,
                            transform * Affine::translate((self.rect.x0 + 14.0, y)),
                            None,
                        );
                        let loc = format!("[Ln {}, Col {}]", d.line, d.col);
                        text_renderer.draw_text(
                            scene,
                            &loc,
                            12.0,
                            c.text_dim,
                            transform * Affine::translate((self.rect.x0 + 36.0, y + 0.5)),
                            None,
                        );
                        text_renderer.draw_text(
                            scene,
                            &d.message,
                            13.0,
                            c.neutral[8],
                            transform * Affine::translate((self.rect.x0 + 130.0, y)),
                            Some((self.rect.width() - 140.0) as f32),
                        );
                    }
                }
            }
            PanelTab::Terminal => {
                if self.has_live_terminal {
                    // The embedded `TerminalWidget` renders the body over
                    // `content_rect()`; leave it to the app so we don't paint
                    // stale text underneath the live grid.
                } else if let Some(note) = &self.terminal_note {
                    text_renderer.draw_text(
                        scene,
                        note,
                        13.0,
                        c.text_dim,
                        transform * Affine::translate((self.rect.x0 + 14.0, content_y0)),
                        Some((self.rect.width() - 28.0) as f32),
                    );
                }
            }
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
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.last_cursor.y >= self.rect.y0
                    && self.last_cursor.y <= self.rect.y0 + TAB_H
                {
                    for (x0, x1, tab) in &self.tab_rects {
                        if self.last_cursor.x >= *x0 && self.last_cursor.x < *x1 {
                            if self.tab != *tab {
                                self.tab = *tab;
                                self.tab_changed = true;
                                self.dirty = true;
                            }
                            *handled = true;
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "BottomPanel"
    }

    fn test_id(&self) -> Option<&str> {
        Some("bottom_panel")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}
