use automerge::{AutoCommit, ObjId, Value, ReadDoc, transaction::Transactable};
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Rect, Point};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;
use crate::i18n;

pub struct Button {
    _id: String,
    label: String,
    /// Accessible name for a button whose visible label is empty (an
    /// icon-only button). Without this the icon carries no meaning for a
    /// screen reader. When the button does show a label, the label is the
    /// accessible name and this is left unset.
    accessible_name: Option<String>,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    /// Optional leading vector glyph drawn instead of (icon-only) or
    /// before (icon + label) the text. Lets toolbars render crisp lucide
    /// icons without depending on a Nerd Font being installed.
    icon: Option<crate::icons::Icon>,
    /// Optional fixed (width, height) override; defaults to 120×40.
    size: Option<(f32, f32)>,
    /// Visual style: neutral (default), primary (accent CTA), or danger.
    /// Drives the fill / border / label colours via [`face_colors`], the
    /// same source of truth the immediate-mode [`Button::paint`] face uses.
    style: ButtonStyle,
    on_click_path: Option<String>, // Path in CRDT to mutate
    /// Optional imperative click handler, fired on release-while-hovered
    /// *in addition to* any `on_click_path` CRDT mutation. Lets app code
    /// that isn't modelling its control flow in the CRDT (e.g. the file
    /// explorer's command sink) react to a press without a message bus.
    on_click: Option<Box<dyn FnMut()>>,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When `false` the button is disabled: skipped by focus, rejects
    /// input (no `on_click`, no CRDT write, no hover/press feedback) and
    /// renders dimmed. Default `true`.
    enabled: bool,
    /// Scene cache. Holds the last emitted body / border / label
    /// primitives at *logical* coordinates; the App's transform
    /// is reapplied on `Scene::append`. See `widget::text` for the
    /// canonical pattern.
    cached: vello::Scene,
    dirty: bool,
    /// Rect the cache was emitted at — compared *by size* to the
    /// current rect; pure translation gets absorbed by the per-
    /// frame append transform without invalidating the cache.
    cache_rect: Rect,
    /// Visual-state snapshot the cache was emitted from. If any
    /// changes we invalidate the cache so the next draw rebuilds.
    last_state: (bool, bool, bool), // (hovered, pressed, focused)
}

impl Button {
    pub fn new(id: &str, label: &str, on_click_path: Option<&str>) -> Self {
        Self {
            _id: id.to_string(),
            label: label.to_string(),
            accessible_name: None,
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            icon: None,
            size: None,
            style: ButtonStyle::Neutral,
            on_click_path: on_click_path.map(|s| s.to_string()),
            on_click: None,
            presence_path: Vec::new(),
            permission_scope: None,
            enabled: true,
            cached: vello::Scene::new(),
            dirty: true,
            cache_rect: Rect::ZERO,
            last_state: (false, false, false),
        }
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    /// An empty path leaves presence opt-out (the default).
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope. When the scope is denied
    /// the widget renders dimmed and rejects input.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Attach an imperative click handler. Fires once per completed
    /// click (left-button release while the cursor is still over the
    /// button), in addition to any `on_click_path` CRDT mutation.
    pub fn on_click<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// Draw `icon` as a leading vector glyph. With an empty label the
    /// icon is centred (icon-only button); with a label it sits to its
    /// left.
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Give an icon-only button a spoken name. The text isn't painted —
    /// it's what a screen reader announces in place of the empty visible
    /// label. Use it for every icon-only toolbar control ("Close",
    /// "Bold", "Add row") so the glyph isn't silent. A button that shows
    /// a label doesn't need this; the label is its accessible name.
    pub fn with_accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    /// Override the default 120×40 intrinsic size — e.g. a compact 40×36
    /// square for a toolbar icon button.
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.size = Some((width, height));
        self
    }

    /// Set the enabled state at build time. A disabled button is skipped
    /// by focus, rejects clicks/keys and renders dimmed. Default `true`.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Imperatively flip the enabled state. Marks the scene cache dirty so
    /// the dimmed/normal look is re-emitted on the next frame.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.dirty = true;
        }
    }

    /// Set the visual [`ButtonStyle`] at build time — `Primary` for the
    /// accent-filled default action (Send / Save), `Danger` for a
    /// destructive one. Colours resolve through [`face_colors`], so the
    /// retained widget and the immediate-mode [`Button::paint`] face stay
    /// pixel-identical for the same style + interaction state.
    pub fn with_style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// Shorthand for [`Self::with_style`]`(ButtonStyle::Primary)` — the
    /// accent-filled primary action.
    pub fn primary(mut self) -> Self {
        self.style = ButtonStyle::Primary;
        self
    }

    /// Shorthand for [`Self::with_style`]`(ButtonStyle::Danger)` — a
    /// destructive action.
    pub fn danger(mut self) -> Self {
        self.style = ButtonStyle::Danger;
        self
    }

    /// Imperatively set the style, invalidating the scene cache so the new
    /// fill/label is re-emitted next frame.
    pub fn set_style(&mut self, style: ButtonStyle) {
        if self.style != style {
            self.style = style;
            self.dirty = true;
        }
    }

    /// Place the button at an absolute rect without going through taffy —
    /// the counterpart to [`TextInput::apply_layout_external`] for an app
    /// that hand-positions a shared control over its own painted chrome. The
    /// button still owns its focus / a11y / activation contract; only its
    /// geometry is caller-driven. Marks the cache dirty on a size change so
    /// the face re-emits at the new dimensions.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        let size_changed =
            rect.width() != self.rect.width() || rect.height() != self.rect.height();
        self.rect = rect;
        if size_changed {
            self.dirty = true;
        }
    }
}

/// Visual style for a button face. Mirrors the conventions the retained
/// [`Button`] and the standard app toolbars already paint by hand:
/// - `Neutral` — a raised surface with a standard border (the default).
/// - `Primary` — an accent-filled default action (Send / Save / Open),
///   white label, no border (the accent fill is self-defining).
/// - `Danger` — a destructive action, danger-filled, white label, no border.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonStyle {
    #[default]
    Neutral,
    Primary,
    Danger,
}

/// Interaction state for an immediate-mode button face. The retained
/// [`Button`] tracks these itself; an immediate-mode caller (a canvas app
/// that owns its own hit-testing) passes what it knows — usually just
/// `hovered`, derived from the last cursor position.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ButtonInteract {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// Resolve the `(fill, border, label)` colours for a button face. The single
/// source of truth for both the retained widget's look and the immediate-mode
/// [`Button::paint`]. `border` is `None` for the accent-filled styles, whose
/// fill is self-defining; the painter adds a focus ring on top when focused.
pub fn face_colors(
    theme: &Theme,
    style: ButtonStyle,
    interact: ButtonInteract,
) -> (vello::peniko::Color, Option<vello::peniko::Color>, vello::peniko::Color) {
    use vello::peniko::Color;
    match style {
        ButtonStyle::Neutral => {
            let bg = if interact.pressed {
                theme.bg_active
            } else if interact.hovered {
                theme.bg_hover
            } else {
                theme.tokens.colors.surfaces.raised
            };
            (bg, Some(theme.tokens.colors.border_standard), theme.tokens.colors.text_primary)
        }
        ButtonStyle::Primary => {
            let bg = if interact.hovered || interact.pressed {
                theme.accent_hover
            } else {
                theme.accent_primary
            };
            (bg, None, Color::WHITE)
        }
        ButtonStyle::Danger => {
            use crate::color_compat::ColorCompat;
            let d = theme.tokens.colors.semantic.danger;
            // Solid danger at rest; lifted ~15% toward white on hover/press so
            // the destructive action gives feedback without a second token.
            let bg = if interact.hovered || interact.pressed {
                let mix = |a: u8| (a as f32 * 0.85 + 255.0 * 0.15).round() as u8;
                Color::from_rgba8(mix(d.r()), mix(d.g()), mix(d.b()), d.a())
            } else {
                d
            };
            (bg, None, Color::WHITE)
        }
    }
}

/// Configuration for [`Button::paint`], the immediate-mode button face. Built
/// fluently: `ButtonFace::new(rect, "Save").primary().hovered(hit)`. Defaults
/// to a neutral, medium-weight, 14px label at an 8px corner radius.
pub struct ButtonFace<'a> {
    rect: Rect,
    label: &'a str,
    icon: Option<crate::icons::Icon>,
    style: ButtonStyle,
    interact: ButtonInteract,
    radius: f64,
    weight: crate::text_render::TextWeight,
    text_size: f32,
    accessible_name: Option<&'a str>,
}

impl<'a> ButtonFace<'a> {
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            icon: None,
            style: ButtonStyle::Neutral,
            interact: ButtonInteract::default(),
            radius: 8.0,
            weight: crate::text_render::TextWeight::Medium,
            text_size: 14.0,
            accessible_name: None,
        }
    }
    pub fn style(mut self, style: ButtonStyle) -> Self { self.style = style; self }
    pub fn primary(mut self) -> Self { self.style = ButtonStyle::Primary; self }
    pub fn danger(mut self) -> Self { self.style = ButtonStyle::Danger; self }
    pub fn icon(mut self, icon: crate::icons::Icon) -> Self { self.icon = Some(icon); self }
    pub fn radius(mut self, radius: f64) -> Self { self.radius = radius; self }
    pub fn weight(mut self, weight: crate::text_render::TextWeight) -> Self { self.weight = weight; self }
    pub fn text_size(mut self, size: f32) -> Self { self.text_size = size; self }
    pub fn hovered(mut self, v: bool) -> Self { self.interact.hovered = v; self }
    pub fn pressed(mut self, v: bool) -> Self { self.interact.pressed = v; self }
    pub fn focused(mut self, v: bool) -> Self { self.interact.focused = v; self }
    pub fn disabled(mut self, v: bool) -> Self { self.interact.disabled = v; self }
    /// Spoken name for an icon-only face (empty visible label). Carried for
    /// callers that expose a their own accessibility node for the region.
    pub fn accessible_name(mut self, name: &'a str) -> Self { self.accessible_name = Some(name); self }
    /// The accessible name a screen reader should announce: the visible label,
    /// or the explicit `accessible_name` when the label is empty.
    pub fn spoken_name(&self) -> &str {
        if !self.label.is_empty() { self.label } else { self.accessible_name.unwrap_or("") }
    }
    /// The style resolved for the current interaction state, as `(fill,
    /// border, label)` colours. Exposed for tests and callers that want the
    /// resolved palette without painting.
    pub fn colors(&self, theme: &Theme) -> (vello::peniko::Color, Option<vello::peniko::Color>, vello::peniko::Color) {
        face_colors(theme, self.style, self.interact)
    }
}

impl Button {
    /// Immediate-mode paint of a themed button face inside `face.rect`,
    /// matching the retained [`Button`]'s look (fill / border / hover / press /
    /// focus-ring / disabled dim, centred label + optional leading icon). For
    /// canvas/immediate-mode apps that own their own rect hit-testing: call
    /// this to paint, keep your existing hit-test to fire the action. Extends
    /// the widget rather than forking a second painter — the retained draw, the
    /// file picker's footer, and every converted app now share `face_colors`.
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        face: &ButtonFace<'_>,
    ) {
        let (bg, border, fg) = face_colors(theme, face.style, face.interact);
        let shape = RoundedRect::from_rect(face.rect, face.radius);

        // Disabled faces dim as one layer so fill + border + label fade
        // together, matching the retained widget's DISABLED_ALPHA look.
        let dim = face.interact.disabled;
        if dim {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }

        scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &shape);
        if face.interact.focused {
            scene.stroke(&vello::kurbo::Stroke::new(2.0), transform, theme.border_focus, None, &shape);
        } else if let Some(bc) = border {
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, bc, None, &shape);
        }

        let rect = face.rect;
        let label = i18n!(face.label);
        let icon_px = (rect.height() - 14.0).clamp(12.0, 22.0);
        match (face.icon, label.is_empty()) {
            (Some(icon), true) => {
                // Icon-only face — centre the glyph.
                let ix = rect.center().x - icon_px / 2.0;
                let iy = rect.center().y - icon_px / 2.0;
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                    icon,
                    fg,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            (icon, _) => {
                // Optional leading icon + centred label cluster.
                let (text_width, _) =
                    text_renderer.measure_weighted(&label, face.text_size, None, face.weight);
                let gap = if icon.is_some() { 6.0 } else { 0.0 };
                let icon_w = if icon.is_some() { icon_px } else { 0.0 };
                let cluster_w = icon_w + gap + text_width as f64;
                let start_x = rect.center().x - cluster_w / 2.0;
                if let Some(icon) = icon {
                    let iy = rect.center().y - icon_px / 2.0;
                    crate::icons::draw_icon(
                        scene,
                        transform,
                        Rect::new(start_x, iy, start_x + icon_px, iy + icon_px),
                        icon,
                        fg,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                let text_x = start_x + icon_w + gap;
                let text_y = rect.y0 + (rect.height() - face.text_size as f64) / 2.0;
                text_renderer.draw_text_weighted(
                    scene,
                    &label,
                    face.text_size,
                    fg,
                    transform * Affine::translate((text_x, text_y)),
                    None,
                    face.weight,
                );
            }
        }

        if dim {
            scene.pop_layer();
        }
    }
}

impl Widget for Button {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Button label could be synced from CRDT here
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let (w, h) = self.size.unwrap_or((120.0, 40.0));
        let node = taffy.new_leaf(Style {
            flex_shrink: 0.0,
            size: Size {
                width: Dimension::length(w),
                height: Dimension::length(h),
            },
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x as f64 + layout.location.x as f64,
            origin.y as f64 + layout.location.y as f64,
            origin.x as f64 + layout.location.x as f64 + layout.size.width as f64,
            origin.y as f64 + layout.location.y as f64 + layout.size.height as f64,
        );
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(&mut self, out_scene: &mut Scene, out_transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let current_state = (self.is_hovered, self.is_pressed, self.is_focused);
        if current_state != self.last_state {
            self.dirty = true;
            self.last_state = current_state;
        }
        let needs_emit = self.dirty || self.cache_rect == Rect::ZERO;
        if !needs_emit {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            let inner = out_transform * Affine::translate((dx, dy));
            if self.enabled {
                out_scene.append(&self.cached, Some(inner));
            } else {
                let clip = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
                out_scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), crate::widget::DISABLED_ALPHA, out_transform, &clip);
                out_scene.append(&self.cached, Some(inner));
                out_scene.pop_layer();
            }
            return;
        }
        // Re-emit at current absolute coords. cache_rect tracks
        // *where* the cache was emitted so subsequent frames can
        // translate without re-encoding.
        self.cached.reset();
        let scene = &mut self.cached;
        let transform = Affine::IDENTITY;
        // Resolve fill / border / label from the shared `face_colors` source
        // of truth so `Neutral` matches the historical look exactly and
        // `Primary`/`Danger` gain the accent / destructive fills. `disabled`
        // is left false here — the dimmed look is applied by the
        // `DISABLED_ALPHA` layer around the cache append, not by the colours.
        let (bg_color, border_opt, fg_color) = face_colors(
            theme,
            self.style,
            ButtonInteract {
                hovered: self.is_hovered,
                pressed: self.is_pressed,
                focused: self.is_focused,
                disabled: false,
            },
        );

        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            &shape,
        );

        // Focus ring wins over the style border; an accent-filled style has
        // no rest border (its fill is self-defining) so it only strokes when
        // focused.
        if self.is_focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &shape,
            );
        } else if let Some(border_color) = border_opt {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                border_color,
                None,
                &shape,
            );
        }

        let text_size = 14.0;
        let label = i18n!(&self.label);
        let icon_px = (self.rect.height() - 14.0).clamp(12.0, 22.0);
        match (self.icon, label.is_empty()) {
            (Some(icon), true) => {
                // Icon-only button — centre the glyph.
                let ix = self.rect.center().x - icon_px / 2.0;
                let iy = self.rect.center().y - icon_px / 2.0;
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(ix, iy, ix + icon_px, iy + icon_px),
                    icon,
                    fg_color,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            (icon, _) => {
                // Optional leading icon + centred label cluster.
                let (text_width, _) = text_renderer.measure(&label, text_size as f32, None);
                let gap = if icon.is_some() { 6.0 } else { 0.0 };
                let icon_w = if icon.is_some() { icon_px } else { 0.0 };
                let cluster_w = icon_w + gap + text_width as f64;
                let start_x = self.rect.center().x - cluster_w / 2.0;
                if let Some(icon) = icon {
                    let iy = self.rect.center().y - icon_px / 2.0;
                    crate::icons::draw_icon(
                        scene,
                        transform,
                        Rect::new(start_x, iy, start_x + icon_px, iy + icon_px),
                        icon,
                        fg_color,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                let text_x = start_x + icon_w + gap;
                let text_y = self.rect.y0 + (self.rect.height() - text_size as f64) / 2.0;
                text_renderer.draw_text(
                    scene,
                    &label,
                    text_size as f32,
                    fg_color,
                    transform * Affine::translate((text_x, text_y)),
                    None,
                );
            }
        }
        // End of cache build — splice the freshly-emitted local
        // scene into the outer App scene with the host transform.
        // delta = 0 here since cache_rect was just set to self.rect.
        self.cache_rect = self.rect;
        if self.enabled {
            out_scene.append(&self.cached, Some(out_transform));
        } else {
            let clip = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
            out_scene.push_layer(vello::peniko::Fill::NonZero, vello::peniko::BlendMode::default(), crate::widget::DISABLED_ALPHA, out_transform, &clip);
            out_scene.append(&self.cached, Some(out_transform));
            out_scene.pop_layer();
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || (self.is_hovered, self.is_pressed, self.is_focused) != self.last_state
    }

    fn mark_drawn(&mut self) { self.dirty = false; }

    /// Button has a fixed 120×40 intrinsic size that doesn't change
    /// between frames — its layout only needs to be computed once.
    /// The `Rect::ZERO` sentinel keeps the first frame's
    /// `compute_layout` running so `apply_layout` can populate the
    /// rect; after that it stays clean (pure translation handled
    /// via the cache's append transform, no taffy involvement).
    fn is_layout_dirty(&self) -> bool { self.cache_rect == Rect::ZERO }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _modifiers: &winit::event::Modifiers, state: &mut AutoCommit, root: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        // Disabled: reject all input — no hover/press feedback, no CRDT
        // write, no callback. Return without setting `handled`.
        if !self.enabled { return; }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical_pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical_pos);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: element_state, button: MouseButton::Left, .. } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *handled = true;

                        if self.is_pressed {
                            *focus_target = Some(node_id);
                        }

                        // CRDT Mutation
                        if !self.is_pressed {
                            if let Some(path) = &self.on_click_path {
                                match state.get(root, path.as_str()) {
                                    Ok(Some((Value::Scalar(s), _))) => {
                                        match s.as_ref() {
                                            automerge::ScalarValue::Counter(_) => {
                                                let _ = state.increment(root, path.as_str(), 1);
                                            }
                                            automerge::ScalarValue::Boolean(_) => {
                                                let _ = state.put(root, path.as_str(), true);
                                            }
                                            _ => {
                                                // Default to setting true if it's some other scalar
                                                let _ = state.put(root, path.as_str(), true);
                                            }
                                        }
                                    }
                                    _ => {
                                        // If it doesn't exist, we'll try to put a boolean true
                                        let _ = state.put(root, path.as_str(), true);
                                    }
                                }
                            }
                            // Imperative handler fires alongside the CRDT path.
                            if let Some(cb) = &mut self.on_click {
                                cb();
                            }
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Button" }

    fn test_id(&self) -> Option<&str> {
        Some(self.label.as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A button is a clickable affordance, so the pointer-finger cursor
    /// while hovered. `_point` is in the same logical space as `rect`.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }
    fn is_enabled(&self) -> bool { self.enabled }

    /// Keyboard / AT activation: Enter, Space, or an AccessKit `Activate`
    /// performs the same CRDT mutation + imperative callback as a mouse click.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        use crate::a11y::Action;
        if !self.enabled {
            return false;
        }
        match action {
            Action::Activate => {
                if let Some(path) = &self.on_click_path {
                    match state.get(root, path.as_str()) {
                        Ok(Some((Value::Scalar(s), _)))
                            if matches!(s.as_ref(), automerge::ScalarValue::Counter(_)) =>
                        {
                            let _ = state.increment(root, path.as_str(), 1);
                        }
                        _ => {
                            let _ = state.put(root, path.as_str(), true);
                        }
                    }
                }
                if let Some(cb) = &mut self.on_click {
                    cb();
                }
                true
            }
            _ => false,
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The visible label is the accessible name. When it's empty (an
        // icon-only button) fall back to the explicit accessible name set
        // via `with_accessible_name`, so the icon isn't silent.
        let label = i18n!(&self.label);
        let name = if !label.is_empty() {
            Some(label)
        } else {
            self.accessible_name
                .as_ref()
                .map(|n| i18n!(n))
                .filter(|n| !n.is_empty())
        };
        Some(
            AccessNode::new(Role::Button)
                .with_optional_name(name)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    ..Default::default()
                }),
        )
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.presence_path.clone() })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) { return; }
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
                scene.fill(vello::peniko::Fill::NonZero, transform, overlay, None, &self.rect);
            }
        }

        if let Some(key) = self.presence_key() {
            if let Some(peers) = presence.here.get(&key) {
                if let Some(user) = peers.first() {
                    presence.draw_peer_focus(scene, theme, user, self.rect);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Widget;

    #[test]
    fn button_test_id_returns_label() {
        let b = Button::new("id", "Save", None);
        assert_eq!(Widget::test_id(&b), Some("Save"));
    }

    #[test]
    fn face_colors_neutral_tracks_hover_and_press() {
        // The immediate-mode face must resolve to the exact tokens the
        // retained widget draws for its neutral look, per interaction state.
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract { hovered: true, ..Default::default() };
        let press = ButtonInteract { pressed: true, ..Default::default() };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Neutral, rest);
        assert_eq!(bg, theme.tokens.colors.surfaces.raised, "rest fill = raised surface");
        assert_eq!(border, Some(theme.tokens.colors.border_standard), "neutral shows a border");
        assert_eq!(fg, theme.tokens.colors.text_primary);
        assert_eq!(face_colors(&theme, ButtonStyle::Neutral, hover).0, theme.bg_hover);
        assert_eq!(face_colors(&theme, ButtonStyle::Neutral, press).0, theme.bg_active);
    }

    #[test]
    fn face_colors_primary_is_accent_borderless_white() {
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract { hovered: true, ..Default::default() };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Primary, rest);
        assert_eq!(bg, theme.accent_primary, "primary rest = accent fill");
        assert_eq!(border, None, "accent fill is self-defining — no border");
        assert_eq!(fg, vello::peniko::Color::WHITE, "accent-filled label is white");
        assert_eq!(
            face_colors(&theme, ButtonStyle::Primary, hover).0,
            theme.accent_hover,
            "primary hover darkens to the accent-hover token",
        );
    }

    #[test]
    fn face_colors_danger_is_solid_at_rest_borderless_white() {
        let theme = Theme::dark();
        let rest = ButtonInteract::default();
        let hover = ButtonInteract { hovered: true, ..Default::default() };
        let (bg, border, fg) = face_colors(&theme, ButtonStyle::Danger, rest);
        assert_eq!(bg, theme.tokens.colors.semantic.danger, "danger rest = solid danger");
        assert_eq!(border, None);
        assert_eq!(fg, vello::peniko::Color::WHITE);
        assert_ne!(
            face_colors(&theme, ButtonStyle::Danger, hover).0,
            theme.tokens.colors.semantic.danger,
            "danger hover lifts off the rest colour for feedback",
        );
    }

    #[test]
    fn button_face_builder_sets_style_and_state() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 80.0, 32.0), "Send")
            .primary()
            .hovered(true)
            .weight(crate::text_render::TextWeight::Bold)
            .text_size(13.0);
        let theme = Theme::dark();
        let (bg, border, fg) = f.colors(&theme);
        assert_eq!(bg, theme.accent_hover);
        assert_eq!(border, None);
        assert_eq!(fg, vello::peniko::Color::WHITE);
        assert_eq!(f.spoken_name(), "Send", "visible label is the spoken name");
    }

    #[test]
    fn icon_only_face_spoken_name_falls_back_to_accessible_name() {
        let f = ButtonFace::new(Rect::new(0.0, 0.0, 32.0, 32.0), "")
            .icon(crate::icons::Icon::Plus)
            .accessible_name("Add page");
        assert_eq!(f.spoken_name(), "Add page");
    }

    #[test]
    fn paint_emits_fill_and_label_geometry() {
        // The immediate-mode painter must actually emit geometry — the body
        // fill plus the label glyphs — so a converted app draws a visible,
        // labelled button.
        use vello_encoding::DrawTag;
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        Button::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &ButtonFace::new(Rect::new(10.0, 10.0, 110.0, 46.0), "Go").primary(),
        );
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(fills > 1, "body fill + label glyphs ⇒ several COLOR tags, got {fills}");
    }

    #[test]
    fn cursor_shape_is_pointer_over_button() {
        // Lay the button out so it has a non-zero hit rect, then assert the
        // pointer-finger cursor is claimed over it and deferred elsewhere.
        let mut b = Button::new("id", "Go", None);
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        assert_eq!(
            Widget::cursor_shape(&b, Point::new(10.0, 10.0)),
            Some(winit::window::CursorIcon::Pointer),
            "pointer over the button requests the pointing-hand cursor",
        );
        assert_eq!(
            Widget::cursor_shape(&b, Point::new(500.0, 500.0)),
            None,
            "pointer well outside the button defers (no cursor claim)",
        );
    }

    #[test]
    fn on_click_callback_fires_on_release_over_button() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go", None).on_click(move || fired.set(fired.get() + 1))
        };

        // Lay the button out so it has a non-zero hit rect.
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut dispatch = |b: &mut Button, ev: WindowEvent| {
            let mut handled = false;
            let mut focus = None;
            b.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &tree);
        };

        // Hover, press, release — the callback fires once on release.
        dispatch(&mut b, WindowEvent::CursorMoved { device_id: device, position: (10.0, 10.0).into() });
        dispatch(&mut b, WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left });
        assert_eq!(fired.get(), 0, "not fired until release");
        dispatch(&mut b, WindowEvent::MouseInput { device_id: device, state: ElementState::Released, button: MouseButton::Left });
        assert_eq!(fired.get(), 1, "fired once on release over the button");
    }

    #[test]
    fn with_enabled_false_is_reported_by_is_enabled() {
        let b = Button::new("id", "Go", None).with_enabled(false);
        assert!(!Widget::is_enabled(&b));
        let b2 = Button::new("id", "Go", None);
        assert!(Widget::is_enabled(&b2), "default is enabled");
    }

    #[test]
    fn accessibility_reports_button_role_name_and_state() {
        use crate::a11y::Role;
        let b = Button::new("id", "Save", None);
        let node = Widget::accessibility(&b).expect("button is not presentational");
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.name.as_deref(), Some("Save"));
        assert!(node.state.focusable);
        assert!(!node.state.focused);
        assert!(!node.state.disabled);
        assert_eq!(node.state.checked, None, "a button is not checkable");
    }

    #[test]
    fn accessibility_reflects_disabled_and_focus() {
        let mut b = Button::new("id", "Save", None).with_enabled(false);
        b.set_focus(true);
        let node = Widget::accessibility(&b).unwrap();
        assert!(node.state.disabled);
        assert!(node.state.focused);
    }

    #[test]
    fn accessibility_icon_only_button_without_name_is_silent() {
        // An icon-only button with no explicit accessible name still has
        // none — the glyph alone carries no spoken meaning.
        let b = Button::new("id", "", None).with_icon(crate::icons::Icon::Check);
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name, None);
    }

    #[test]
    fn accessibility_icon_only_button_uses_accessible_name() {
        // `with_accessible_name` supplies the spoken name for an icon-only
        // button so a screen reader announces "Confirm" instead of nothing.
        let b = Button::new("id", "", None)
            .with_icon(crate::icons::Icon::Check)
            .with_accessible_name("Confirm");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Confirm"));
    }

    #[test]
    fn accessibility_visible_label_wins_over_accessible_name() {
        // When a button shows a label, that label is the accessible name —
        // an accessible-name override is only the icon-only fallback.
        let b = Button::new("id", "Save", None).with_accessible_name("Persist");
        let node = Widget::accessibility(&b).unwrap();
        assert_eq!(node.name.as_deref(), Some("Save"));
    }

    #[test]
    fn disabled_button_rejects_input() {
        use std::cell::Cell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton};

        let fired = Rc::new(Cell::new(0u32));
        let mut b = {
            let fired = fired.clone();
            Button::new("id", "Go", Some("clicked"))
                .on_click(move || fired.set(fired.get() + 1))
                .with_enabled(false)
        };

        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = b.build_layout(&mut tree, &theme, &mut node_map);
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(200.0),
                height: taffy::AvailableSpace::Definite(60.0),
            },
        )
        .unwrap();
        b.apply_layout(&tree, node, Point::ZERO);

        let device: DeviceId = unsafe { std::mem::zeroed() };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut focus = None;
        let mut handled = false;

        for ev in [
            WindowEvent::CursorMoved { device_id: device, position: (10.0, 10.0).into() },
            WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left },
            WindowEvent::MouseInput { device_id: device, state: ElementState::Released, button: MouseButton::Left },
        ] {
            b.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, &tree);
        }

        assert_eq!(fired.get(), 0, "disabled button must not fire on_click");
        assert!(focus.is_none(), "disabled button must not request focus");
        assert!(!handled, "disabled button must not swallow input");
        assert!(
            doc.get(&root, "clicked").unwrap().is_none(),
            "disabled button must not write the CRDT path"
        );
    }
}
