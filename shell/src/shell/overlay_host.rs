//! Shell overlay host — wraps the root content widget and draws modal
//! overlays (command palette for ⌘K, overview for ⌘Tab, etc.) on top.
//!
//! The `build_nirios_root` entry point returns this widget so that the
//! shell has exactly one place to route global keyboard events
//! (⌘K / ⌘Tab / Esc) and exactly one place to render the overlay stack
//! above the desktop content. Overlays each own their state — the host
//! just gates their visibility and paints a scrim when at least one is
//! showing.
//!
//! Phase 3 scope: ⌘K opens the command palette primitive. Overview /
//! quick-settings / workspace-settings wiring lands in follow-up tasks
//! alongside their click-surfaces.
//!
//! Key routing: `handle_event` consumes the palette-toggle and Esc
//! presses before propagating to the wrapped content. When the palette
//! is open, it also gets first crack at every event so typing-into-
//! query / Enter-to-select work without the desktop stealing focus.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Rect as TaffyRect, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;
use quick_ui::{CommandEntry, CommandPalette};

pub struct ShellOverlayHost {
    rect: Rect,
    content: Box<dyn Widget>,
    palette: CommandPalette,
    palette_visible: bool,
    // Child node ids are captured once during `build_layout` so we can
    // forward layout/draw/event calls without re-walking the tree.
    content_node: Option<NodeId>,
    palette_node: Option<NodeId>,
}

impl ShellOverlayHost {
    pub fn new(content: Box<dyn Widget>) -> Self {
        let mut palette = CommandPalette::new();
        for entry in [
            CommandEntry::new("search.open",        "Open global search"),
            CommandEntry::new("overview.toggle",    "Toggle overview"),
            CommandEntry::new("settings.open",      "Open workspace settings"),
            CommandEntry::new("quick-settings.open", "Open quick settings"),
        ] {
            palette.push(entry);
        }
        Self {
            rect: Rect::ZERO,
            content,
            palette,
            palette_visible: false,
            content_node: None,
            palette_node: None,
        }
    }

    fn toggle_palette(&mut self) {
        self.palette_visible = !self.palette_visible;
    }
}

impl Widget for ShellOverlayHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        self.content.sync(s, p);
        self.palette.sync(s, p);
    }

    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        let a = self.content.update(s, p);
        let b = self.palette.update(s, p);
        a || b
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let content_node = self.content.build_layout(taffy, theme, node_map);
        let palette_node = self.palette.build_layout(taffy, theme, node_map);
        // Overlay the palette above content: absolute positioning with
        // auto-margin horizontal centering and a fixed top offset so it
        // floats below the shell's top bar like a popover.
        let mut palette_style = taffy.style(palette_node).unwrap().clone();
        palette_style.position = Position::Absolute;
        palette_style.margin = TaffyRect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::auto(),
        };
        palette_style.inset = TaffyRect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(120.0),
            bottom: LengthPercentageAuto::auto(),
        };
        taffy.set_style(palette_node, palette_style).unwrap();

        self.content_node = Some(content_node);
        self.palette_node = Some(palette_node);
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[content_node, palette_node],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let children = taffy.children(node).unwrap();
        self.content
            .apply_layout(taffy, children[0], Point::new(self.rect.x0, self.rect.y0));
        self.palette
            .apply_layout(taffy, children[1], Point::new(self.rect.x0, self.rect.y0));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        if self.palette_visible {
            // Scrim behind the overlay (concept: `bg-black/60` + 8px blur).
            // We skip the blur for now — Vello doesn't ship a backdrop-blur
            // node yet — and use a flat dark alpha instead.
            scene.fill(
                Fill::NonZero,
                transform,
                Color::rgba8(0, 0, 0, 154),
                None,
                &self.rect,
            );
            self.palette
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Global shortcuts first — ⌘K / Ctrl+K toggles the palette,
        // Esc closes it. These are consumed regardless of whether the
        // wrapped content would also react.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent {
                logical_key,
                state: ElementState::Pressed,
                ..
            },
            ..
        } = event
        {
            let is_mod = modifiers.state().super_key() || modifiers.state().control_key();
            if is_mod {
                if let Key::Character(s) = logical_key {
                    if s.as_str().eq_ignore_ascii_case("k") {
                        self.toggle_palette();
                        *handled = true;
                        return;
                    }
                }
            }
            if matches!(logical_key, Key::Named(NamedKey::Escape)) && self.palette_visible {
                self.palette_visible = false;
                *handled = true;
                return;
            }
        }

        let children = taffy.children(node_id).unwrap();
        if self.palette_visible {
            self.palette.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                children[1],
                taffy,
            );
            if *handled {
                return;
            }
        }
        self.content.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            children[0],
            taffy,
        );
    }

    fn name(&self) -> &str {
        "ShellOverlayHost"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
