//! The active workspace's strips as named places in the bar.
//!
//! A host that knows who is where hands the bar [`StripPlaces`]; the bar
//! then lists the workspace's strips beside its name ("Games · 3") when
//! there is more than one, and moves to one on a click. A new desk and the
//! workspace's new name, asked for in the workspace menu, are typed in
//! place here. Without places nothing here paints or takes input.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Point, Rect, RoundedRect, Scene, TextRenderer, TextRole,
    Theme, TopBarChrome, at, paint_bar_selector, t, t_args,
};
use crate::chrome::StripPlaces;
use crate::text_entry::{self, Edit, Mods};
use tilo_desktop_model::{DesktopTree, StripId};
use ui::text_render::TextStyle;
use ui::text_selection::Selection;
use ui::widget::{ControlSize, FieldFace, TextInput, Widget};
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
    field_rect: Rect,
    naming: Option<Naming>,
}

/// The strip name being typed.
struct Naming {
    /// What the name is for.
    purpose: Purpose,
    text: String,
    selection: Selection,
    input: Option<TextInput>,
}

/// What a name being typed in the bar is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// A new desk (strip).
    NewDesk,
    /// The workspace's own name.
    RenameWorkspace,
}

impl Purpose {
    fn placeholder(self) -> String {
        t(match self {
            Self::NewDesk => "topbar-strip-name-placeholder",
            Self::RenameWorkspace => "topbar-workspace-name-placeholder",
        })
    }
}

/// Everything about the strips that changes what the bar paints.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct StripsKey {
    strips: Vec<(StripId, String, usize)>,
    active: Option<StripId>,
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
    const CONTROL_STRIP_MORE: &'static str = "topbar:strip-more";
    const STRIP_TARGET_PREFIX: &'static str = "topbar:strip:";

    /// Who is on which strip, when the host shows strips as places.
    pub(super) fn strip_places(&self) -> Option<StripPlaces> {
        self.chrome.as_ref()?.snapshot().strip_places
    }

    /// The places the bar shows as desks: only when there is more than one
    /// desk to be on, or a name is being typed; adding one is in the
    /// workspace menu.
    fn shown_places(&self) -> Option<StripPlaces> {
        let places = self.strip_places()?;
        if self.strips.strips.len() <= 1 && self.strips.naming.is_none() {
            return None;
        }
        Some(places)
    }

    /// Start naming a new desk, as the workspace menu's New desk asks;
    /// false when the host does not let this person add one.
    pub fn begin_new_desk(&mut self) -> bool {
        let started = self.begin_strip_naming();
        self.cache_fp = super::CacheFingerprint::default();
        started
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

    /// The shown strip's name, when the bar names it after the workspace
    /// pill ("Home ⌄ › Research"): whenever no strip chips stand there (the
    /// native desktop, or a single desk), and only for a strip with a name.
    pub(super) fn strip_name(&self) -> Option<&str> {
        if self.shown_places().is_some() {
            return None;
        }
        let active = self.strips.active?;
        self.strips
            .strips
            .iter()
            .find(|(id, _)| *id == active)
            .map(|(_, name)| name.trim())
            .filter(|name| !name.is_empty())
    }

    /// The gap either side of the separator before the strip's name.
    fn strip_name_gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s1)
    }

    /// The width the strip's name takes after the pill at its natural
    /// length: the separator, its gaps and the name; zero when the bar
    /// names no strip.
    fn strip_name_natural_width(&self, text_renderer: &mut TextRenderer, theme: &Theme) -> f64 {
        let Some(name) = self.strip_name() else {
            return 0.0;
        };
        let style = Self::strip_text_style(theme);
        let separator = text_renderer
            .measure_style(super::STRIP_SEPARATOR, None, &style)
            .0;
        let name = text_renderer.measure_style(name, None, &style).0;
        f64::from(separator + name) + 2.0 * Self::strip_name_gap(theme)
    }

    /// Paint the shown strip's name between `x0` and `x1`, after the
    /// workspace pill: the separator dim, the name elided to the room left.
    /// The separator and the name are one target, which opens the strip
    /// switcher ([`ChromeIntent::ShowStrips`]).
    pub(super) fn draw_strip_name(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        span: (f64, f64),
    ) {
        self.ws_strip_rect = Rect::ZERO;
        let Some(name) = self.strip_name().map(str::to_owned) else {
            return;
        };
        let (x0, x1) = span;
        let style = Self::strip_text_style(theme);
        let gap = Self::strip_name_gap(theme);
        let (separator_w, separator_h) =
            text_renderer.measure_style(super::STRIP_SEPARATOR, None, &style);
        let name_x = x0 + gap + f64::from(separator_w) + gap;
        if x1 - name_x < BAR_CONTROL_PX {
            return;
        }
        let name =
            text_renderer.elide_to_width_style(&name, ui::num::f32_of_f64(x1 - name_x), &style);
        let (name_w, name_h) = text_renderer.measure_style(&name, None, &style);
        let colors = &theme.tokens.colors;
        text_renderer.draw_style(
            scene,
            super::STRIP_SEPARATOR,
            colors.text_dim,
            at(
                transform,
                (x0 + gap, center_y - f64::from(separator_h) / 2.0),
            ),
            None,
            &style,
        );
        text_renderer.draw_style(
            scene,
            &name,
            colors.text_primary,
            at(transform, (name_x, center_y - f64::from(name_h) / 2.0)),
            None,
            &style,
        );
        self.ws_strip_rect = Rect::new(
            x0,
            center_y - BAR_CONTROL_PX / 2.0,
            name_x + f64::from(name_w) + gap,
            center_y + BAR_CONTROL_PX / 2.0,
        );
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
            naming: self
                .strips
                .naming
                .as_ref()
                .map(|n| (n.text.clone(), n.selection.anchor, n.selection.cursor)),
        }
    }

    #[cfg(test)]
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
        let Some(places) = self.shown_places() else {
            return self.strip_name_natural_width(text_renderer, theme);
        };
        if self.strips.naming.is_some() {
            return FIELD_MAX_PX + BAR_CONTROL_PX;
        }
        self.strips
            .strips
            .iter()
            .map(|(id, name)| {
                Self::strip_chip_width(text_renderer, theme, name, places.people_on(*id))
            })
            .sum()
    }

    /// Paint the strips between `x0` and `x1`: every strip that fits, the
    /// active one first to stay. While a name is being typed, the field
    /// takes the chips' place.
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
        self.strips.field_rect = Rect::ZERO;
        let Some(places) = self.shown_places() else {
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
            Self::fit_strips_with_more(text_renderer, theme, &widths, active, x1 - x0);
        let more_w = more.map_or(0.0, |(_, _, width)| width);
        let used: f64 = widths
            .iter()
            .zip(&shown)
            .filter(|(_, shown)| **shown)
            .map(|(width, _)| width)
            .sum();
        if used <= 0.0 && more.is_none() {
            return;
        }
        let group = Rect::new(
            x0,
            center_y - BAR_CONTROL_PX / 2.0,
            x0 + used + more_w,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        let radius = ui::chrome::ChromeRole::Selector.radius(theme, group.height());
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
        }
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
            paint_bar_selector(
                scene,
                transform,
                &RoundedRect::from_rect(chip, radius),
                ui::chrome::SelectionState::active(true),
                theme,
            );
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
        let placeholder = naming.purpose.placeholder();
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

    fn begin_strip_naming(&mut self) -> bool {
        if !self.strip_places().is_some_and(|places| places.can_add) {
            return false;
        }
        self.strips.naming = Some(Naming {
            purpose: Purpose::NewDesk,
            text: String::new(),
            selection: Selection::collapsed(0),
            input: None,
        });
        true
    }

    /// Start renaming the workspace in place, from its current name, as the
    /// workspace menu's Rename asks.
    pub fn begin_workspace_rename(&mut self) {
        let text = self.workspace_name.clone();
        let end = text.len();
        self.strips.naming = Some(Naming {
            purpose: Purpose::RenameWorkspace,
            text,
            selection: Selection {
                anchor: 0,
                cursor: end,
            },
            input: None,
        });
        self.cache_fp = super::CacheFingerprint::default();
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
            chrome.intent(match naming.purpose {
                Purpose::NewDesk => ChromeIntent::AddStrip { name },
                Purpose::RenameWorkspace => ChromeIntent::RenameWorkspace { name },
            });
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

    /// A left press at `point`: a chip moves there, a press elsewhere ends
    /// naming. Returns whether it was ours.
    pub(super) fn strip_press(&mut self, point: Point) -> bool {
        if self.strips.naming.is_some() {
            if self.strips.field_rect.contains(point) {
                return true;
            }
            self.cancel_strip_naming();
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
        self.strips
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
        rects.push(("topbar_strip_name_field".into(), self.strips.field_rect));
        rects
    }

    fn strip_target(strip: StripId) -> String {
        format!("{}{}", Self::STRIP_TARGET_PREFIX, strip.0)
    }

    /// The semantic action for a strip control, `None` when `target` is
    /// not one.
    pub(super) fn strip_semantic_action(&mut self, target: &str) -> Option<bool> {
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
        let Some(places) = self.shown_places() else {
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
        if let Some(naming) = self.strips.naming.as_ref() {
            nodes.push(
                AccessNode::new(Role::TextInput)
                    .with_hashed_key("topbar-strip-name")
                    .with_name(naming.purpose.placeholder())
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
            if candidate.strips.more.is_some() && !candidate.strips.chips.is_empty() {
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

    /// One desk is no choice to show, adding one is in the workspace menu,
    /// and both a new desk and the workspace's new name are typed in place
    /// in the bar.
    #[test]
    fn desks_show_when_several_and_are_named_in_place() {
        let mut one = tree();
        one.workspaces[0].strips.truncate(1);
        let chrome = Rc::new(Host {
            tree: RefCell::new(one.clone()),
            reported: Some(StripPlaces::in_active_workspace(&one, &[])),
            intents: RefCell::default(),
        });
        let one_desk =
            TopBarChrome::from_tree_with_local_user(&one, "me".into()).with_chrome(chrome.clone());
        let mut frame = Framed::new(ui::State::new(), one_desk, (1440, 480), Theme::dark());
        assert!(frame.strips.chips.is_empty(), "one desk is no choice");

        assert!(frame.begin_new_desk());
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(!frame.strips.field_rect.is_zero_area());
        let field = frame.strips.field_rect.center();
        Framed::harness_mut(&mut frame).click_at(field.x, field.y);
        Framed::harness_mut(&mut frame).type_text("Games");
        Framed::harness_mut(&mut frame).key("Enter");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().last(),
            Some(&ChromeIntent::AddStrip {
                name: "Games".into()
            })
        );

        frame.begin_workspace_rename();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(!frame.strips.field_rect.is_zero_area());
        // The current name is selected: typing replaces it.
        let field = frame.strips.field_rect.center();
        Framed::harness_mut(&mut frame).click_at(field.x, field.y);
        Framed::harness_mut(&mut frame).type_text("Lounge");
        Framed::harness_mut(&mut frame).key("Enter");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().last(),
            Some(&ChromeIntent::RenameWorkspace {
                name: "Lounge".into()
            })
        );

        let (several, _) = bar(Some(places()), 1600);
        assert_eq!(several.strips.chips.len(), 3, "several desks are shown");
    }

    /// Without the host's leave the workspace menu offers no new desk, and
    /// starting one anyway does nothing.
    #[test]
    fn a_new_desk_needs_the_hosts_leave() {
        let (mut frame, _) = bar(
            Some(StripPlaces {
                can_add: false,
                ..places()
            }),
            1600,
        );
        assert!(!frame.begin_new_desk());
        assert!(!frame.strip_naming());
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
    }

    #[test]
    fn strips_sit_between_the_workspace_and_the_centre_and_switch_on_click() {
        let (mut frame, chrome) = bar(Some(places()), 1600);
        let pill = frame.ws_pill_rect;
        let center = frame.center_rect;
        let main = rect(&frame, "topbar_strip_Main");
        let games = rect(&frame, "topbar_strip_Games");
        for chip in [main, games] {
            assert!(!chip.is_zero_area());
            assert!(chip.x0 > pill.x1, "{chip:?} after {pill:?}");
            assert!(chip.x1 < center.x0, "{chip:?} before {center:?}");
        }
        assert!(main.x1 <= games.x0);
        Framed::harness_mut(&mut frame).click_at(games.center().x, games.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().as_slice(),
            [ChromeIntent::SwitchStrip(StripId(11))]
        );
    }

    #[test]
    fn a_new_desk_is_named_inline() {
        let (mut frame, chrome) = bar(Some(places()), 1600);
        assert!(frame.begin_new_desk());
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert!(frame.strip_naming());
        let field = rect(&frame, "topbar_strip_name_field");
        Framed::harness_mut(&mut frame).click_at(field.center().x, field.center().y);
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
        for typed in ["Later", ""] {
            assert!(frame.begin_new_desk());
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            let field = rect(&frame, "topbar_strip_name_field");
            Framed::harness_mut(&mut frame).click_at(field.center().x, field.center().y);
            if typed.is_empty() {
                Framed::harness_mut(&mut frame).key("Enter");
            } else {
                Framed::harness_mut(&mut frame).type_text(typed);
                Framed::harness_mut(&mut frame).key("Escape");
            }
            Framed::harness_mut(&mut frame).poll_frame().unwrap();
            assert!(!frame.strip_naming());
        }
        assert!(chrome.intents.borrow().is_empty());
    }

    #[test]
    fn a_narrow_bar_keeps_its_desks_named() {
        let (frame, _) = bar(Some(places()), 360);
        assert!(!frame.strips.chips.is_empty() || frame.strips.more.is_some());
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
