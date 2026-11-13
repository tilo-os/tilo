//! The window's reactions at the row's end: the shared reaction strip
//! ([`crate::widget::reaction_strip`]), a pill per emoji and the add button, after the app's slots
//! and before the window's own controls. They are the window's, not the app's
//! ([`crate::window_toolbar::show_reactions`]): a press on a pill, or a pick in the shared emoji
//! picker the add button opens, goes back as [`crate::window_toolbar::reaction_action`].
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::{CONTROL_PX, SECTION_GAP, Slot, ToolbarRow, Widget};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::DrawChild;
use crate::widget::emoji_picker::{EmojiPicker, PickerEvent};
use crate::widget::reaction_strip::{self, ReactionStrip, StripMetrics};
use crate::window_toolbar::{ToolbarEvent, ToolbarReaction, WindowToolbar, reaction_action};

pub const REACTION_TARGET_PREFIX: &str = "window-toolbar-reaction:";
pub const REACTION_ADD_TARGET: &str = "window-toolbar-reaction-add";

type MeasureKey = (Vec<(String, u32, bool)>, bool, u32);

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ReactionsMeasure {
    key: Option<MeasureKey>,
    pills: Vec<f64>,
    add: Option<f64>,
    gap: f64,
    height: f64,
}

pub(super) struct RowReactions {
    measure: ReactionsMeasure,
    strip: ReactionStrip,
    picker: Box<EmojiPicker>,
    opening: Option<Rect>,
}

impl Default for RowReactions {
    fn default() -> Self {
        Self {
            measure: ReactionsMeasure::default(),
            strip: ReactionStrip::new(),
            picker: Box::new(EmojiPicker::framed()),
            opening: None,
        }
    }
}

const REACTION_SHOWER: &str = "\u{1}you";

fn group(pill: &ToolbarReaction) -> tilo_resource::ReactionGroup {
    let named = |by: &str, name: Option<&str>| tilo_resource::Reactor {
        by: by.to_owned(),
        persona: None,
        name: name.map(str::to_owned),
    };
    let mut reactors: Vec<tilo_resource::Reactor> = Vec::new();
    if pill.mine {
        reactors.push(named(REACTION_SHOWER, None));
    }
    reactors.extend(pill.names.iter().map(|name| named("", Some(name))));
    let count = usize::try_from(pill.count)
        .unwrap_or(usize::MAX)
        .min(10_000);
    while reactors.len() < count {
        reactors.push(named("", Some("")));
    }
    tilo_resource::ReactionGroup {
        emoji: pill.emoji.clone(),
        reactors,
        first_at: 0,
    }
}

fn groups(toolbar: &WindowToolbar, count: usize) -> Vec<tilo_resource::ReactionGroup> {
    toolbar
        .reactions
        .as_ref()
        .map(|reactions| reactions.pills.iter().take(count).map(group).collect())
        .unwrap_or_default()
}

/// Where the reactions measured `measure` stand at the end of `row`, and
/// the row left before them for the app's slots: the pills that fit in a
/// third of the row (one at most where `compact`), then the add button.
/// Nothing where there are none.
pub(super) fn place(
    measure: &ReactionsMeasure,
    row: Rect,
    compact: bool,
) -> (Rect, Vec<Rect>, Rect) {
    if measure.key.is_none() || (measure.pills.is_empty() && measure.add.is_none()) {
        return (row, Vec::new(), Rect::ZERO);
    }
    let add = measure.add.unwrap_or(0.0);
    let first = measure.pills.first().copied().unwrap_or(0.0);
    // At least the add button, or the first pill of someone who may not
    // add one, whatever the row's width.
    let least = if measure.add.is_some() { add } else { first };
    let budget = (row.width() / 3.0).max(least);
    let most = if compact { 1 } else { measure.pills.len() };
    let mut width = add;
    let mut shown: usize = 0;
    for pill in measure.pills.iter().take(most) {
        let with = width + pill + if width > 0.0 { measure.gap } else { 0.0 };
        if with > budget && (shown > 0 || measure.add.is_some()) {
            break;
        }
        width = with;
        shown = shown.saturating_add(1);
    }
    if width <= 0.0 {
        return (row, Vec::new(), Rect::ZERO);
    }
    let y0 = row.center().y - measure.height / 2.0;
    let mut x = row.x1 - width;
    let app = Rect::new(row.x0, row.y0, (x - SECTION_GAP).max(row.x0), row.y1);
    let pills = measure
        .pills
        .iter()
        .take(shown)
        .map(|pill| {
            let rect = Rect::new(x, y0, x + pill, y0 + measure.height);
            x = rect.x1 + measure.gap;
            rect
        })
        .collect();
    let add = measure.add.map_or(Rect::ZERO, |add| {
        Rect::new(x, y0, x + add, y0 + measure.height)
    });
    (app, pills, add)
}

impl ToolbarRow {
    pub(super) fn measure_reactions(&mut self, theme: &Theme) -> bool {
        let reactions = self
            .toolbar()
            .and_then(|toolbar| toolbar.reactions.as_ref());
        let key = reactions.map(|reactions| {
            (
                reactions
                    .pills
                    .iter()
                    .map(|pill| (pill.emoji.clone(), pill.count, pill.mine))
                    .collect::<Vec<_>>(),
                reactions.can_react,
                theme.text_scale.to_bits(),
            )
        });
        if key == self.reactions.measure.key {
            return false;
        }
        let measured = match (key, reactions) {
            (Some(key), Some(reactions)) => {
                let groups = groups(
                    self.toolbar().unwrap_or(&WindowToolbar::default()),
                    usize::MAX,
                );
                let metrics = StripMetrics::of(theme);
                let laid = crate::text_render::with_layout_renderer(theme, |text| {
                    reaction_strip::layout(
                        &groups,
                        REACTION_SHOWER,
                        reactions.can_react,
                        (Point::ZERO, f64::MAX / 4.0),
                        text,
                        &metrics,
                    )
                });
                ReactionsMeasure {
                    key: Some(key),
                    pills: laid.pills().iter().map(|pill| pill.rect.width()).collect(),
                    add: laid.add_rect().map(|add| add.width()),
                    gap: f64::from(theme.tokens.spacing.s1),
                    height: metrics.height().min(CONTROL_PX),
                }
            }
            _ => ReactionsMeasure::default(),
        };
        let changed = measured != self.reactions.measure;
        self.reactions.measure = measured;
        changed
    }

    pub(super) fn place_reactions(&self, row: Rect) -> (Rect, Vec<Rect>, Rect) {
        place(&self.reactions.measure, row, self.compact)
    }

    pub(super) fn may_react(&self) -> bool {
        self.toolbar()
            .and_then(|toolbar| toolbar.reactions.as_ref())
            .is_some_and(|reactions| reactions.can_react)
    }

    fn pill_emoji(&self, index: usize) -> Option<String> {
        self.toolbar()?
            .reactions
            .as_ref()?
            .pills
            .get(index)
            .map(|pill| pill.emoji.clone())
    }

    pub(super) fn react(&mut self, emoji: &str) -> bool {
        if !self.may_react() {
            return false;
        }
        if !self
            .toolbar()
            .and_then(|t| t.reactions.as_ref())
            .is_some_and(|r| r.pills.iter().any(|pill| pill.emoji == emoji && pill.mine))
        {
            crate::emoji_catalogue::record_recent(emoji);
        }
        self.push(ToolbarEvent::Action {
            id: reaction_action(emoji),
        });
        true
    }

    pub(super) fn activate_reaction(&mut self, slot: &Slot) -> bool {
        match slot {
            Slot::Reaction(i) => match self.pill_emoji(*i) {
                Some(emoji) => self.react(&emoji),
                None => false,
            },
            Slot::ReactionAdd => self.open_picker(),
            _ => false,
        }
    }

    pub(super) fn open_picker(&mut self) -> bool {
        let anchor = self.slots.reaction_add;
        if !self.may_react() || anchor.area() <= 0.0 {
            return false;
        }
        self.menu.close();
        self.reactions.opening = Some(anchor);
        true
    }

    pub(super) fn picking(&self) -> bool {
        self.reactions.opening.is_some() || self.reactions.picker.is_open()
    }

    pub(super) fn close_picker(&mut self) {
        self.reactions.opening = None;
        self.reactions.picker.close();
    }

    pub(super) fn picker_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.reactions.picker.is_open() {
            return false;
        }
        let pointer = matches!(
            event,
            WindowEvent::MouseInput { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        let mut handled = false;
        self.reactions.picker.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        for picked in self.reactions.picker.take_events() {
            match picked {
                PickerEvent::Picked(emoji) => {
                    self.reactions.picker.close();
                    self.react(&emoji);
                }
                PickerEvent::Dismissed => self.reactions.picker.close(),
            }
        }
        if handled || !pointer {
            *ctx.handled = true;
            return true;
        }
        false
    }

    pub(super) fn paint_picker(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) {
        if let Some(anchor) = self.reactions.opening.take() {
            let area = if self.popup_area.area() > 0.0 {
                self.popup_area
            } else {
                self.row
            };
            self.reactions.picker.open_at(Some(anchor), area, theme);
        }
        if self.reactions.picker.is_open() {
            DrawChild::draw_child(
                &mut *self.reactions.picker,
                scene,
                transform,
                tr,
                theme,
                presence,
            );
        }
    }

    pub(super) fn paint_reactions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme): (&mut TextRenderer, &Theme),
    ) {
        let slots = &self.slots;
        let Some(origin) = slots
            .reactions
            .first()
            .copied()
            .or_else(|| (slots.reaction_add.area() > 0.0).then_some(slots.reaction_add))
        else {
            return;
        };
        let end = if slots.reaction_add.area() > 0.0 {
            slots.reaction_add.x1
        } else {
            slots.reactions.last().map_or(origin.x1, |last| last.x1)
        };
        let Some(toolbar) = self.shown.as_ref().map(|(_, toolbar)| toolbar) else {
            return;
        };
        let groups = groups(toolbar, slots.reactions.len());
        let can_react = slots.reaction_add.area() > 0.0;
        let hover = match &self.hover {
            Some(Slot::Reaction(i)) => slots.reactions.get(*i).map(|rect| rect.center()),
            Some(Slot::ReactionAdd) => Some(slots.reaction_add.center()),
            _ => None,
        };
        self.reactions.strip.layout(
            &groups,
            REACTION_SHOWER,
            can_react,
            (origin.origin(), end - origin.x0 + 1.0),
            tr,
            theme,
        );
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        self.reactions.strip.paint(&mut ctx, hover, None);
    }

    pub(super) fn reaction_tooltip(&self, p: Point) -> Option<(Rect, String)> {
        self.reactions
            .strip
            .tooltip_at(p, &|reactor| reactor.name.clone().unwrap_or_default())
    }

    pub(super) fn reaction_nodes(&self) -> Vec<crate::a11y::AccessNode> {
        let mut nodes: Vec<crate::a11y::AccessNode> = self
            .reactions
            .strip
            .accessibility()
            .into_iter()
            .enumerate()
            .map(|(i, node)| {
                let target = match self
                    .pill_emoji(i)
                    .filter(|_| i < self.slots.reactions.len())
                {
                    Some(emoji) => format!("{REACTION_TARGET_PREFIX}{emoji}"),
                    None => REACTION_ADD_TARGET.to_owned(),
                };
                node.with_action_target(target)
            })
            .collect();
        if self.reactions.picker.is_open()
            && let Some(picker) = self.reactions.picker.accessibility()
        {
            nodes.push(picker);
        }
        nodes
    }

    pub(super) fn perform_reaction(&mut self, target: &str) -> Option<bool> {
        if target == REACTION_ADD_TARGET {
            return Some(self.open_picker());
        }
        let emoji = target.strip_prefix(REACTION_TARGET_PREFIX)?;
        Some(!emoji.is_empty() && self.react(emoji))
    }

    pub(super) fn reactions_moving(&self) -> bool {
        self.reactions.strip.is_moving()
            || self.picking() && self.reactions.picker.needs_animation_frame()
    }
}
