//! The active workspace's strips as named places in the bar.
//!
//! A host that knows who is where hands the bar [`StripPlaces`]; the bar
//! then lists the workspace's strips beside its name ("Games · 3"), moves
//! to one on a click and offers adding a named strip. Without places (the
//! native desktop, whose strip ribbon already places strips) nothing here
//! paints or takes input.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Point, Rect, RoundedRect, SMALL_GLYPH_PX, Scene,
    TextRenderer, TextRole, Theme, TopBarChrome, at, bar_selector_body, t, t_args,
};
use crate::chrome::StripPlaces;
use crate::text_entry::{self, Edit, Mods};
use tilo_desktop_model::{DesktopTree, StripId};
use ui::text_render::TextStyle;
use ui::text_selection::Selection;
use ui::widget::{ControlSize, FieldFace, TextInput, Widget};
use vello::peniko::Fill;
use winit::event::WindowEvent;

/// Longest strip name the bar accepts, in characters.
const NAME_MAX_CHARS: usize = 48;

/// Widest the naming field grows, before the room the bar has.
const FIELD_MAX_PX: f64 = 200.0;

#[derive(Default)]
pub(super) struct StripSwitcher {
    /// The active workspace's strips, in order.
    strips: Vec<(StripId, String)>,
    active: Option<StripId>,
    /// Chips as last painted, for hit testing.
    chips: Vec<(StripId, Rect)>,
    /// The chip standing for the strips with no room: how many, the one it
    /// moves to, and where it was painted.
    more: Option<(usize, StripId, Rect)>,
    add_rect: Rect,
    field_rect: Rect,
    naming: Option<Naming>,
}

/// The strip name being typed.
struct Naming {
    text: String,
    selection: Selection,
    input: Option<TextInput>,
}

/// Everything about the strips that changes what the bar paints.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct StripsKey {
    strips: Vec<(StripId, String, usize)>,
    active: Option<StripId>,
    can_add: bool,
    naming: Option<(String, usize, usize)>,
}

/// One chip's text: the name, and the people count when anyone is there.
fn chip_text(name: &str, count: usize) -> (String, Option<String>) {
    let count = (count > 0).then(|| format!(" · {count}"));
    (name.to_owned(), count)
}

/// Which strips fit in `room`: the active one first, so it always stays
/// when it fits at all, then the others in order while they fit.
fn fit_strips(widths: &[f64], active: Option<usize>, room: f64) -> Vec<bool> {
    let mut room = room;
    let mut shown = vec![false; widths.len()];
    let order = active
        .into_iter()
        .chain((0..widths.len()).filter(|index| Some(*index) != active));
    for index in order {
        if let (Some(width), Some(slot)) = (widths.get(index), shown.get_mut(index))
            && *width <= room
        {
            *slot = true;
            room -= width;
        }
    }
    shown
}

/// The strip the "more" chip moves to: the first one not shown after the
/// active one, wrapping, so pressing it again walks through all of them.
fn next_hidden(shown: &[bool], active: Option<usize>) -> Option<usize> {
    let start = active.map_or(0, |index| index.saturating_add(1));
    (start..shown.len())
        .chain(0..start.min(shown.len()))
        .find(|index| shown.get(*index) == Some(&false))
}

impl TopBarChrome {
    pub(super) const CONTROL_STRIP_ADD: &'static str = "topbar:strip-add";
    const CONTROL_STRIP_MORE: &'static str = "topbar:strip-more";
    const STRIP_TARGET_PREFIX: &'static str = "topbar:strip:";

    /// Who is on which strip, when the host shows strips as places.
    pub(super) fn strip_places(&self) -> Option<StripPlaces> {
        self.chrome.as_ref()?.snapshot().strip_places
    }

    pub(super) fn sync_strips(&mut self, tree: &DesktopTree) {
        let workspace = tree.workspaces.iter().find(|w| w.id == tree.active);
        self.strips.strips = workspace
            .map(|w| {
                w.strips
                    .iter()
                    .map(|strip| (strip.id, strip.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.strips.active = tree
            .active_strip
            .get(&tree.active)
            .copied()
            .filter(|id| self.strips.strips.iter().any(|(strip, _)| strip == id))
            .or_else(|| self.strips.strips.first().map(|(id, _)| *id));
    }

    pub(super) fn strips_key(&self) -> StripsKey {
        let Some(places) = self.strip_places() else {
            return StripsKey::default();
        };
        StripsKey {
            strips: self
                .strips
                .strips
                .iter()
                .map(|(id, name)| (*id, name.clone(), places.people_on(*id)))
                .collect(),
            active: self.strips.active,
            can_add: places.can_add,
            naming: self
                .strips
                .naming
                .as_ref()
                .map(|n| (n.text.clone(), n.selection.anchor, n.selection.cursor)),
        }
    }

    pub(super) fn strip_naming(&self) -> bool {
        self.strips.naming.is_some()
    }

    fn strip_text_style(theme: &Theme) -> TextStyle {
        theme.tokens.typography.style(TextRole::Caption)
    }

    /// The width a chip for `name` with `count` people takes.
    fn strip_chip_width(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        name: &str,
        count: usize,
    ) -> f64 {
        let style = Self::strip_text_style(theme);
        let (name, count) = chip_text(name, count);
        let pad = f64::from(theme.tokens.spacing.s3);
        let name_w = text_renderer.measure_style(&name, None, &style).0;
        let count_w = count.map_or(0.0, |count| {
            text_renderer.measure_style(&count, None, &style).0
        });
        f64::from(name_w + count_w) + 2.0 * pad
    }

    /// The widest the "+N" chip gets with up to `most` strips hidden.
    /// Which strips fit in `room`, and when some do not, the "+N" chip
    /// standing for them: how many, the one it moves to, and its width.
    fn fit_strips_with_more(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        widths: &[f64],
        active: Option<usize>,
        room: f64,
    ) -> (Vec<bool>, Option<(usize, usize, f64)>) {
        let shown = fit_strips(widths, active, room);
        if !shown.contains(&false) {
            return (shown, None);
        }
        let label = t_args(
            "topbar-strips-more-label",
            &[("count", widths.len().into())],
        );
        let more_w = Self::strip_chip_width(text_renderer, theme, &label, 0);
        let shown = fit_strips(widths, active, room - more_w);
        let hidden = shown.iter().filter(|shown| !**shown).count();
        let more = next_hidden(&shown, active).map(|next| (hidden, next, more_w));
        (shown, more)
    }

    /// What the switcher takes when every strip fits, so the workspace
    /// name can leave it room. Zero without places.
    pub(super) fn strips_natural_width(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let Some(places) = self.strip_places() else {
            return 0.0;
        };
        if self.strips.naming.is_some() {
            return FIELD_MAX_PX + BAR_CONTROL_PX;
        }
        let chips: f64 = self
            .strips
            .strips
            .iter()
            .map(|(id, name)| {
                Self::strip_chip_width(text_renderer, theme, name, places.people_on(*id))
            })
            .sum();
        chips + if places.can_add { BAR_CONTROL_PX } else { 0.0 }
    }

    /// Paint the strips between `x0` and `x1`: every strip that fits, the
    /// active one first to stay, then the add control. While a name is
    /// being typed, the field takes the chips' place.
    pub(super) fn draw_strip_places(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        span: (f64, f64),
    ) {
        self.strips.chips.clear();
        self.strips.more = None;
        self.strips.add_rect = Rect::ZERO;
        self.strips.field_rect = Rect::ZERO;
        let Some(places) = self.strip_places() else {
            return;
        };
        let (x0, x1) = span;
        if x1 - x0 < BAR_CONTROL_PX {
            return;
        }
        if self.strips.naming.is_some() {
            let field = Rect::new(
                x0,
                center_y - FieldFace::height(ControlSize::Regular, theme) / 2.0,
                (x0 + FIELD_MAX_PX).min(x1),
                center_y + FieldFace::height(ControlSize::Regular, theme) / 2.0,
            );
            self.paint_strip_field(scene, transform, theme, text_renderer, field);
            return;
        }
        let add_w = if places.can_add { BAR_CONTROL_PX } else { 0.0 };
        let widths: Vec<f64> = self
            .strips
            .strips
            .iter()
            .map(|(id, name)| {
                Self::strip_chip_width(text_renderer, theme, name, places.people_on(*id))
            })
            .collect();
        let active = self
            .strips
            .strips
            .iter()
            .position(|(id, _)| Some(*id) == self.strips.active);
        // Whatever has no room stays one press away, through a "+N" chip.
        let (shown, more) =
            Self::fit_strips_with_more(text_renderer, theme, &widths, active, x1 - x0 - add_w);
        let more_w = more.map_or(0.0, |(_, _, width)| width);
        let used: f64 = widths
            .iter()
            .zip(&shown)
            .filter(|(_, shown)| **shown)
            .map(|(width, _)| width)
            .sum();
        if used <= 0.0 && add_w <= 0.0 && more.is_none() {
            return;
        }
        let group = Rect::new(
            x0,
            center_y - BAR_CONTROL_PX / 2.0,
            x0 + used + more_w + add_w,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        let radius = ui::chrome::ChromeRole::Selector.radius(theme, group.height());
        let rest = bar_selector_body(theme, ui::chrome::SelectionState::active(false));
        ui::chrome::shell::paint_glass_body(scene, transform, group, radius, theme, &rest, 1.0);
        let mut x = x0;
        let strips = self.strips.strips.clone();
        for (((id, name), width), shown) in strips.iter().zip(&widths).zip(&shown) {
            if !shown {
                continue;
            }
            let chip = Rect::new(x, group.y0, x + width, group.y1);
            let active = Some(*id) == self.strips.active;
            Self::paint_strip_chip(
                scene,
                transform,
                theme,
                text_renderer,
                (chip, radius),
                (name, places.people_on(*id), active),
            );
            self.strips.chips.push((*id, chip));
            x += width;
        }
        if let Some((hidden, next, width)) = more
            && let Some((next, _)) = strips.get(next)
        {
            let chip = Rect::new(x, group.y0, x + width, group.y1);
            let label = t_args("topbar-strips-more-label", &[("count", hidden.into())]);
            Self::paint_strip_chip(
                scene,
                transform,
                theme,
                text_renderer,
                (chip, radius),
                (&label, 0, false),
            );
            self.strips.more = Some((hidden, *next, chip));
            x += width;
        }
        if places.can_add {
            let add = Rect::new(x, group.y0, x + BAR_CONTROL_PX, group.y1);
            Self::paint_strip_add(scene, transform, theme, add);
            self.strips.add_rect = add;
        }
    }

    /// The add control: a plus in dim ink.
    fn paint_strip_add(scene: &mut Scene, transform: Affine, theme: &Theme, add: Rect) {
        let glyph = SMALL_GLYPH_PX;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(add.center(), (glyph, glyph)),
            ui::icons::Icon::Plus,
            theme.tokens.colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// One strip chip: the active one lifted, its name, then the count in
    /// dim ink.
    fn paint_strip_chip(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (chip, radius): (Rect, f64),
        (name, count, active): (&str, usize, bool),
    ) {
        let colors = &theme.tokens.colors;
        if active {
            let body = bar_selector_body(theme, ui::chrome::SelectionState::active(true));
            let shape = RoundedRect::from_rect(chip, radius);
            scene.fill(Fill::NonZero, transform, body.fill, None, &shape);
            if let Some(edge) = body.border {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    edge,
                    None,
                    &shape,
                );
            }
        }
        let style = Self::strip_text_style(theme);
        let pad = f64::from(theme.tokens.spacing.s3);
        let (label, count) = chip_text(name, count);
        let (name_w, name_h) = text_renderer.measure_style(&label, None, &style);
        let y = chip.center().y - f64::from(name_h) / 2.0;
        let ink = if active {
            colors.text_primary
        } else {
            colors.text_dim
        };
        text_renderer.draw_style(
            scene,
            &label,
            ink,
            at(transform, (chip.x0 + pad, y)),
            None,
            &style,
        );
        if let Some(count) = count {
            text_renderer.draw_style(
                scene,
                &count,
                colors.text_dim,
                at(transform, (chip.x0 + pad + f64::from(name_w), y)),
                None,
                &style,
            );
        }
    }

    fn paint_strip_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        field: Rect,
    ) {
        self.strips.field_rect = field;
        let Some(naming) = self.strips.naming.as_mut() else {
            return;
        };
        let placeholder = t("topbar-strip-name-placeholder");
        let face = FieldFace::new(field, &naming.text)
            .placeholder(&placeholder)
            .text_role(TextRole::Caption)
            .focused(true);
        if naming.text.is_empty() {
            face.paint(scene, transform, text_renderer, theme);
        } else {
            face.paint_chrome(scene, transform, theme);
        }
        let input = naming.input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Caption)
        });
        input.set_text(&naming.text);
        input.set_selection(naming.selection.anchor, naming.selection.cursor);
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(theme));
        let input_theme = theme
            .clone()
            .with_text_scale(theme.text_scale * theme.ui_scale);
        input.draw(
            scene,
            transform,
            text_renderer,
            &input_theme,
            &ui::presence::PresenceCtx::default(),
        );
    }

    /// Paint the naming field in the compressed command row's place.
    pub(super) fn draw_responsive_strip_field(
        &mut self,
        scene: &mut Scene,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        if self.strips.naming.is_none() {
            return;
        }
        let row = self.responsive.toolbar.get_rect();
        let height = FieldFace::height(ControlSize::Regular, theme);
        let center_y = self.rect.center().y;
        let field = Rect::new(
            row.x0,
            center_y - height / 2.0,
            row.x1.max(row.x0 + BAR_CONTROL_PX),
            center_y + height / 2.0,
        );
        self.paint_strip_field(scene, Affine::IDENTITY, theme, text_renderer, field);
    }

    pub(super) fn begin_strip_naming(&mut self) -> bool {
        if !self.strip_places().is_some_and(|places| places.can_add) {
            return false;
        }
        self.strips.naming = Some(Naming {
            text: String::new(),
            selection: Selection::collapsed(0),
            input: None,
        });
        true
    }

    pub(super) fn cancel_strip_naming(&mut self) -> bool {
        self.strips.naming.take().is_some()
    }

    fn commit_strip_naming(&mut self) {
        let Some(naming) = self.strips.naming.take() else {
            return;
        };
        let name: String = naming.text.trim().chars().take(NAME_MAX_CHARS).collect();
        if name.is_empty() {
            return;
        }
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::AddStrip { name });
        }
    }

    pub(super) fn switch_strip(&mut self, strip: StripId) -> bool {
        if Some(strip) == self.strips.active {
            return true;
        }
        let moved = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.intent(ChromeIntent::SwitchStrip(strip)));
        // Lit at once; the host's next tree confirms it.
        if moved {
            self.strips.active = Some(strip);
        }
        moved
    }

    /// A left press at `point`: a chip moves there, the add control starts
    /// naming, a press elsewhere ends naming. Returns whether it was ours.
    pub(super) fn strip_press(&mut self, point: Point) -> bool {
        if self.strips.naming.is_some() {
            if self.strips.field_rect.contains(point) {
                return true;
            }
            self.cancel_strip_naming();
        }
        if self.strips.add_rect.contains(point) {
            return self.begin_strip_naming();
        }
        if let Some((_, next, rect)) = self.strips.more
            && rect.contains(point)
        {
            return self.switch_strip(next);
        }
        let chip = self
            .strips
            .chips
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, _)| *id);
        chip.is_some_and(|strip| self.switch_strip(strip))
    }

    /// Keys while a name is typed. Every key is the field's; returns
    /// whether the event was one.
    pub(super) fn strip_naming_key(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(naming) = self.strips.naming.as_mut() else {
            return false;
        };
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != winit::event::ElementState::Pressed {
            return true;
        }
        let mods = Mods::from_state(modifiers.state());
        match text_entry::apply(
            &key.logical_key,
            mods,
            &mut naming.text,
            &mut naming.selection,
        ) {
            Edit::Submit => self.commit_strip_naming(),
            Edit::Cancel => {
                self.cancel_strip_naming();
            }
            Edit::Changed => {
                if naming.text.chars().count() > NAME_MAX_CHARS {
                    naming.text = naming.text.chars().take(NAME_MAX_CHARS).collect();
                    let end = naming.text.len();
                    naming.selection = Selection::collapsed(end);
                }
            }
            Edit::Ignored | Edit::ConsumedQuiet => {}
        }
        true
    }

    /// Whether a pointer at `point` is over a strip control.
    pub(super) fn strip_hit(&self, point: Point) -> bool {
        self.strips.add_rect.contains(point)
            || self
                .strips
                .more
                .is_some_and(|(_, _, rect)| rect.contains(point))
            || self
                .strips
                .chips
                .iter()
                .any(|(_, rect)| rect.contains(point))
    }

    pub(super) fn strip_test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects: Vec<(String, Rect)> = self
            .strips
            .chips
            .iter()
            .filter_map(|(id, rect)| {
                let (_, name) = self.strips.strips.iter().find(|(strip, _)| strip == id)?;
                Some((format!("topbar_strip_{name}"), *rect))
            })
            .collect();
        rects.push((
            "topbar_strip_more".into(),
            self.strips.more.map_or(Rect::ZERO, |(_, _, rect)| rect),
        ));
        rects.push(("topbar_strip_add".into(), self.strips.add_rect));
        rects.push(("topbar_strip_name_field".into(), self.strips.field_rect));
        rects
    }

    fn strip_target(strip: StripId) -> String {
        format!("{}{}", Self::STRIP_TARGET_PREFIX, strip.0)
    }

    /// The semantic action for a strip control, `None` when `target` is
    /// not one.
    pub(super) fn strip_semantic_action(&mut self, target: &str) -> Option<bool> {
        if target == Self::CONTROL_STRIP_ADD {
            return Some(self.begin_strip_naming());
        }
        if target == Self::CONTROL_STRIP_MORE {
            return Some(
                self.strips
                    .more
                    .is_some_and(|(_, next, _)| self.switch_strip(next)),
            );
        }
        let id: u64 = target
            .strip_prefix(Self::STRIP_TARGET_PREFIX)?
            .parse()
            .ok()?;
        Some(self.switch_strip(StripId(id)))
    }

    /// Accessibility nodes for the strips as painted.
    pub(super) fn strip_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let Some(places) = self.strip_places() else {
            return Vec::new();
        };
        let mut nodes: Vec<AccessNode> = self
            .strips
            .chips
            .iter()
            .filter_map(|(id, rect)| {
                let (_, name) = self.strips.strips.iter().find(|(strip, _)| strip == id)?;
                Some(
                    AccessNode::new(Role::ToggleButton)
                        .with_action_target(Self::strip_target(*id))
                        .with_name(t_args(
                            "topbar-strip-go",
                            &[
                                ("name", name.as_str().into()),
                                ("count", places.people_on(*id).into()),
                            ],
                        ))
                        .with_bounds(*rect)
                        .with_state(AccessState {
                            focusable: true,
                            checked: Some(Some(*id) == self.strips.active),
                            ..Default::default()
                        }),
                )
            })
            .collect();
        if let Some((hidden, next, rect)) = self.strips.more
            && let Some((_, name)) = self.strips.strips.iter().find(|(id, _)| *id == next)
        {
            nodes.push(
                AccessNode::new(Role::Button)
                    .with_action_target(Self::CONTROL_STRIP_MORE)
                    .with_name(t_args(
                        "topbar-strips-more",
                        &[("count", hidden.into()), ("name", name.as_str().into())],
                    ))
                    .with_bounds(rect)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            );
        }
        if !self.strips.add_rect.is_zero_area() {
            nodes.push(
                AccessNode::new(Role::Button)
                    .with_action_target(Self::CONTROL_STRIP_ADD)
                    .with_name(t("topbar-strip-add"))
                    .with_bounds(self.strips.add_rect)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            );
        }
        if let Some(naming) = self.strips.naming.as_ref() {
            nodes.push(
                AccessNode::new(Role::TextInput)
                    .with_hashed_key("topbar-strip-name")
                    .with_name(t("topbar-strip-name-placeholder"))
                    .with_value(naming.text.clone())
                    .with_bounds(self.strips.field_rect)
                    .with_state(AccessState {
                        focusable: true,
                        focused: true,
                        ..Default::default()
                    }),
            );
        }
        nodes
    }

    /// The strips as entries of the compressed command row, labelled with
    /// who is there; the add control last.
    pub(super) fn strip_responsive_specs(&self) -> Vec<super::responsive::Spec> {
        use super::responsive::{Command, Spec};
        let Some(places) = self.strip_places() else {
            return Vec::new();
        };
        let mut specs: Vec<Spec> = self
            .strips
            .strips
            .iter()
            .map(|(id, name)| {
                let count = places.people_on(*id);
                Spec {
                    command: Command::Strip(*id),
                    name: if count > 0 {
                        t_args(
                            "topbar-strip-count",
                            &[("name", name.as_str().into()), ("count", count.into())],
                        )
                    } else {
                        name.clone()
                    },
                    icon: ui::icons::Icon::Layers,
                    priority: 15,
                    enabled: true,
                    active: Some(Some(*id) == self.strips.active),
                    dot: false,
                    mark: false,
                }
            })
            .collect();
        if places.can_add {
            specs.push(Spec {
                command: Command::AddStrip,
                name: t("topbar-strip-add"),
                icon: ui::icons::Icon::Plus,
                priority: 14,
                enabled: true,
                active: None,
                dot: false,
                mark: false,
            });
        }
        specs
    }
}

impl TopBarChrome {
    /// While a strip name is typed, the field owns the keyboard and a press
    /// anywhere else ends it. Returns whether the event was consumed.
    pub(super) fn handle_strip_naming_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.strips.naming.is_none() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Some(ctx.logical(position));
                false
            }
            WindowEvent::KeyboardInput { .. } => {
                let consumed = self.strip_naming_key(event, *ctx.modifiers);
                *ctx.handled |= consumed;
                self.responsive.dirty = true;
                consumed
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            } => {
                let inside = self
                    .last_cursor
                    .is_some_and(|point| self.strips.field_rect.contains(point));
                if inside {
                    *ctx.handled = true;
                    return true;
                }
                self.cancel_strip_naming();
                self.responsive.dirty = true;
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{DesktopChromeSnapshot, DesktopChromeSource};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_desktop_model::{Strip, Workspace, WorkspaceId};
    use ui::headless::Framed;

    struct Host {
        tree: RefCell<DesktopTree>,
        reported: Option<StripPlaces>,
        intents: RefCell<Vec<ChromeIntent>>,
    }

    impl DesktopChromeSource for Host {
        fn snapshot(&self) -> DesktopChromeSnapshot {
            DesktopChromeSnapshot {
                strip_places: self.reported.clone(),
                ..Default::default()
            }
        }
        fn tree(&self) -> DesktopTree {
            self.tree.borrow().clone()
        }
        fn intent(&self, intent: ChromeIntent) -> bool {
            self.intents.borrow_mut().push(intent);
            true
        }
    }

    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.strips = vec![
            Strip::new(StripId(10), "Main"),
            Strip::new(StripId(11), "Games"),
            Strip::new(StripId(12), "Quiet"),
        ];
        let mut tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        tree.active_strip.insert(WorkspaceId(1), StripId(10));
        tree
    }

    fn bar(places: Option<StripPlaces>, width: u32) -> (Framed<TopBarChrome>, Rc<Host>) {
        let tree = tree();
        let chrome = Rc::new(Host {
            tree: RefCell::new(tree.clone()),
            reported: places,
            intents: RefCell::default(),
        });
        let bar =
            TopBarChrome::from_tree_with_local_user(&tree, "me".into()).with_chrome(chrome.clone());
        let frame = Framed::new(ui::State::new(), bar, (width, 480), Theme::dark());
        (frame, chrome)
    }

    fn rect(frame: &Framed<TopBarChrome>, name: &str) -> Rect {
        frame
            .test_rects()
            .into_iter()
            .find(|(key, _)| key == name)
            .map_or(Rect::ZERO, |(_, rect)| rect)
    }

    fn places() -> StripPlaces {
        StripPlaces {
            present: vec![(StripId(10), 1), (StripId(11), 3)],
            can_add: true,
        }
    }

    #[test]
    fn active_strip_stays_when_room_runs_out() {
        assert_eq!(
            fit_strips(&[50.0, 50.0, 50.0], Some(2), 100.0),
            [true, false, true]
        );
        assert_eq!(fit_strips(&[50.0, 50.0], None, 60.0), [true, false]);
        assert_eq!(fit_strips(&[80.0], Some(0), 60.0), [false]);
    }

    #[test]
    fn more_walks_the_hidden_strips_after_the_active_one() {
        assert_eq!(next_hidden(&[true, false, true, false], Some(2)), Some(3));
        assert_eq!(next_hidden(&[false, true, true], Some(2)), Some(0));
        assert_eq!(next_hidden(&[true, true], Some(0)), None);
    }

    /// With no room for every strip, a "+N" chip stands for the rest and
    /// moves to the next one, so every strip is reachable from the bar.
    #[test]
    fn strips_without_room_are_one_press_away() {
        let mut frame = None;
        // The narrowest wide bar that still shows the switcher at all.
        for width in (900..1600).step_by(20) {
            let (candidate, chrome) = bar(Some(places()), width);
            if !candidate.responsive.active
                && candidate.strips.more.is_some()
                && !candidate.strips.chips.is_empty()
            {
                frame = Some((candidate, chrome));
                break;
            }
        }
        let (mut frame, chrome) = frame.expect("some width hides a strip");
        let (hidden, next, _) = frame.strips.more.unwrap();
        assert!(hidden >= 1);
        assert!(frame.strips.chips.iter().all(|(id, _)| *id != next));
        let more = rect(&frame, "topbar_strip_more");
        Framed::harness_mut(&mut frame).click_at(more.center().x, more.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().as_slice(),
            [ChromeIntent::SwitchStrip(next)]
        );
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }

    #[test]
    fn counts_show_only_where_people_are() {
        assert_eq!(chip_text("Games", 3), ("Games".into(), Some(" · 3".into())));
        assert_eq!(chip_text("Quiet", 0), ("Quiet".into(), None));
    }

    #[test]
    fn without_places_the_bar_shows_no_strips() {
        let (frame, _) = bar(None, 1600);
        assert!(rect(&frame, "topbar_strip_Main").is_zero_area());
        assert!(rect(&frame, "topbar_strip_add").is_zero_area());
    }

    #[test]
    fn strips_sit_between_the_workspace_and_the_centre_and_switch_on_click() {
        let (mut frame, chrome) = bar(Some(places()), 1600);
        let pill = frame.ws_pill_rect;
        let center = frame.center_rect;
        let main = rect(&frame, "topbar_strip_Main");
        let games = rect(&frame, "topbar_strip_Games");
        let add = rect(&frame, "topbar_strip_add");
        for chip in [main, games, add] {
            assert!(!chip.is_zero_area());
            assert!(chip.x0 > pill.x1, "{chip:?} after {pill:?}");
            assert!(chip.x1 < center.x0, "{chip:?} before {center:?}");
        }
        assert!(main.x1 <= games.x0 && games.x1 <= add.x0);
        Framed::harness_mut(&mut frame).click_at(games.center().x, games.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().as_slice(),
            [ChromeIntent::SwitchStrip(StripId(11))]
        );
    }

    #[test]
    fn adding_a_strip_names_it_inline() {
        let (mut frame, chrome) = bar(Some(places()), 1600);
        let add = rect(&frame, "topbar_strip_add");
        Framed::harness_mut(&mut frame).click_at(add.center().x, add.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(frame.strip_naming());
        assert!(!rect(&frame, "topbar_strip_name_field").is_zero_area());
        Framed::harness_mut(&mut frame).type_text("  Games night ");
        Framed::harness_mut(&mut frame).key("Enter");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(!frame.strip_naming());
        assert_eq!(
            chrome.intents.borrow().as_slice(),
            [ChromeIntent::AddStrip {
                name: "Games night".into()
            }]
        );
    }

    #[test]
    fn escape_or_an_empty_name_adds_nothing() {
        let (mut frame, chrome) = bar(Some(places()), 1600);
        let add = rect(&frame, "topbar_strip_add");
        Framed::harness_mut(&mut frame).click_at(add.center().x, add.center().y);
        Framed::harness_mut(&mut frame).type_text("Later");
        Framed::harness_mut(&mut frame).key("Escape");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(!frame.strip_naming());
        Framed::harness_mut(&mut frame).click_at(add.center().x, add.center().y);
        Framed::harness_mut(&mut frame).key("Enter");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(chrome.intents.borrow().is_empty());
    }

    #[test]
    fn narrow_bar_lists_strips_with_counts_in_its_command_row() {
        let (frame, _) = bar(Some(places()), 360);
        assert!(frame.responsive.active);
        let names: Vec<String> = frame
            .strip_responsive_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert_eq!(names, ["Main · 1", "Games · 3", "Quiet", "Add a strip"]);
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }

    #[test]
    fn wide_bar_with_strips_passes_the_native_audit() {
        let (frame, _) = bar(Some(places()), 1600);
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }
}
