//! The floating layer as the person meets it: an activity's card drawn in the
//! toolkit's floating frame, the presses on its controls, dragging it loose
//! and snapping it to an edge, and the group assistive technology reads.
//!
//! The layer decides nothing about the activities themselves. It shows the
//! cards it is handed ([`floating_layer::Card`] from the session), places
//! them with the toolkit's pure layout ([`tilo_ui::floating_layer`]) and puts
//! what the person does as [`Ask`]s, the session's to carry out.

use ui::a11y::{AccessNode, AccessState, Role};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect, Vec2};
use ui::widget::ActivityCardFace;
use ui::widget::floating_surface::{
    paint_card_body, paint_card_edge, paint_card_shadow, painted_bounds,
};
use ui::{
    floating_layer::{
        Card, Corner, EDGE_INSET, FoldReason, Layer, Placed, Placement, card_parts, place,
    },
    live_activity::Summary,
};

/// What the person did on the layer, for the session to carry out.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// They pressed a control the activity offers: `control` is its id.
    Control {
        app: String,
        activity: String,
        control: String,
    },
    /// They asked for the activity to end, or to go away until it changes.
    Close { app: String, activity: String },
    /// They folded the card back into the bar's pill.
    Collapse { app: String, activity: String },
    /// They left the card somewhere, or put it back in the column.
    Moved {
        app: String,
        activity: String,
        at: Option<Rect>,
    },
    /// Keyboard focus entered the layer, or left it.
    Focus(bool),
}

/// Whether `card` stands on screen exactly as something in `cards` does.
fn drawn_like(cards: &[Placed], card: &Placed) -> bool {
    cards.iter().any(|other| {
        other.app == card.app
            && other.summary.id == card.summary.id
            && other.summary == card.summary
            && other.rect == card.rect
    })
}

/// A drag in flight: which card, and where the pointer took it from.
struct Drag {
    app: String,
    activity: String,
    offset: Vec2,
    /// The card's rect as it is dragged, in the layer's coordinates.
    rect: Rect,
    /// The pointer had moved far enough to call it a drag rather than a press.
    moved: bool,
}

/// The layer's drawing and input, over the cards the session publishes.
#[derive(Default)]
pub struct FloatingLayer {
    placement: Placement,
    corner: Corner,
    cards: Vec<Card>,
    /// The phone-width card the person expanded out of the bar.
    expanded: bool,
    drag: Option<Drag>,
    /// The card whose control a press began on, until the pointer lifts.
    press: Option<(String, String, String)>,
    /// The card the keyboard is on, by app and activity.
    focus: Option<(String, String)>,
    /// Where the cards stand as the screen shows them: what the next frame
    /// compares its own against.
    painted: Vec<Drawn>,
    /// The cards that arrived, left, moved, changed or were pressed since the
    /// last frame: the next frame damages their ground and nothing else.
    changed: Vec<Rect>,
    /// What the last frame changed, shadows and all.
    damage: Rect,
    /// The next frame is owed to the screen.
    owed: bool,
    layout: Layer,
}

/// A card as the last frame drew it.
#[derive(Clone, Debug, PartialEq)]
struct Drawn {
    app: String,
    activity: String,
    summary: Summary,
    rect: Rect,
}

impl FloatingLayer {
    /// Show the cards `cards` on `placement`, docking them in `corner`.
    pub fn set(&mut self, placement: Placement, corner: Corner, cards: Vec<Card>) -> bool {
        let changed = self.placement != placement || self.corner != corner || self.cards != cards;
        if changed {
            self.placement = placement;
            self.corner = corner;
            self.cards = cards;
            self.relayout();
        }
        changed
    }

    /// Expand the phone-width card the person pulled out of the bar, or
    /// collapse it back.
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.relayout();
        }
    }

    #[must_use]
    pub fn expanded(&self) -> bool {
        self.expanded
    }

    fn relayout(&mut self) {
        let layout = place(&self.placement, self.corner, &self.cards, self.expanded);
        // A drag whose card is no longer shown ends with it.
        if let Some(drag) = &self.drag
            && !layout
                .cards
                .iter()
                .any(|card| card.app == drag.app && card.summary.id == drag.activity)
        {
            self.drag = None;
        }
        // A card that arrived, went, moved or changed repaints where it was
        // and where it is; the others stay as the screen has them.
        for card in &self.layout.cards {
            if !drawn_like(&layout.cards, card) {
                self.changed.push(self.dragged(card));
            }
        }
        for card in &layout.cards {
            if !drawn_like(&self.layout.cards, card) {
                self.changed.push(card.rect);
            }
        }
        self.layout = layout;
        self.owed = true;
    }

    /// Where the cards stand now.
    #[must_use]
    pub fn layout(&self) -> &Layer {
        &self.layout
    }

    /// What the layer shows, for the host's hit testing and assistive
    /// technology.
    #[must_use]
    pub fn shown(&self) -> Shown {
        Shown {
            cards: self
                .layout
                .cards
                .iter()
                .map(|card| (card.app.clone(), card.summary.id.clone(), card.rect))
                .collect(),
            folded: self
                .layout
                .folded
                .iter()
                .filter(|folded| folded.reason == FoldReason::Phone)
                .count(),
            keyboard: self.focus.is_some(),
            dragging: self.drag.as_ref().is_some_and(|drag| drag.moved),
            damage: self.damage,
        }
    }

    /// Whether the layer takes a point: the cards' shapes, never their
    /// shadows, so a press beside a card reaches the window under it.
    #[must_use]
    pub fn accepts(&self, point: Point) -> bool {
        self.layout
            .cards
            .iter()
            .any(|card| card.rect.contains(point))
    }

    /// Draw the cards. Everything else on the surface stays clear.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        for card in &self.layout.cards {
            let rect = self.dragged(card);
            paint_card_shadow(scene, transform, rect, 1.0, theme);
            paint_card_body(scene, transform, rect, theme);
            self.paint_card(scene, transform, text_renderer, theme, rect, card);
            paint_card_edge(scene, transform, rect, theme);
        }
        // The frame changes the ground of what changed, shadows and all, and
        // nothing else: a card that only ticks damages its own card.
        let mut damage: Option<Rect> = None;
        for rect in std::mem::take(&mut self.changed) {
            let bounds = painted_bounds(rect, theme);
            damage = Some(damage.map_or(bounds, |damage| damage.union(bounds)));
        }
        self.damage = damage.unwrap_or(Rect::ZERO);
        self.painted = self
            .layout
            .cards
            .iter()
            .map(|card| Drawn {
                app: card.app.clone(),
                activity: card.summary.id.clone(),
                summary: card.summary.clone(),
                rect: self.dragged(card),
            })
            .collect();
        self.owed = false;
    }

    /// Keyboard focus left or reached the cards at `indices`: each repaints.
    fn refocused(&mut self, indices: &[usize]) {
        for index in indices {
            if let Some(card) = self.layout.cards.get(*index) {
                self.changed.push(card.rect);
            }
        }
    }

    fn dragged(&self, card: &Placed) -> Rect {
        self.drag
            .as_ref()
            .filter(|drag| drag.app == card.app && drag.activity == card.summary.id)
            .map_or(card.rect, |drag| drag.rect)
    }

    fn paint_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        card: &Placed,
    ) {
        let focused = self
            .focus
            .as_ref()
            .is_some_and(|(app, activity)| *app == card.app && *activity == card.summary.id)
            && self.press.is_none();
        let held = self
            .press
            .as_ref()
            .filter(|(app, activity, _)| *app == card.app && *activity == card.summary.id)
            .map(|(_, _, control)| control.as_str());
        ActivityCardFace {
            summary: &card.summary,
            collapse_label: &crate::i18n::t("activity-collapse"),
            close_label: &crate::i18n::t("activity-close"),
            focused,
            held,
        }
        .paint(scene, transform, text, theme, rect);
    }

    /// The part of the surface the last frame changed.
    #[must_use]
    pub fn damage(&self) -> Rect {
        self.damage
    }

    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        self.owed
    }

    /// The pointer moved to `point`.
    pub fn pointer_motion(&mut self, point: Point) -> bool {
        let Some(drag) = &mut self.drag else {
            return false;
        };
        let origin = Point::new(point.x - drag.offset.x, point.y - drag.offset.y);
        if !drag.moved && origin.distance(origin_of(drag.rect)) < 4.0 {
            return false;
        }
        drag.moved = true;
        self.changed.push(drag.rect);
        drag.rect = Rect::from_origin_size(origin, drag.rect.size());
        self.changed.push(drag.rect);
        self.owed = true;
        true
    }

    /// A pointer button went down (`true`) or up at `point`.
    pub fn pointer_button(&mut self, point: Point, pressed: bool) -> Vec<Ask> {
        if pressed {
            return self.press_at(point);
        }
        self.release_at(point)
    }

    fn press_at(&mut self, point: Point) -> Vec<Ask> {
        let Some(card) = self
            .layout
            .cards
            .iter()
            .rev()
            .find(|card| card.rect.contains(point))
            .cloned()
        else {
            return Vec::new();
        };
        self.focus = Some((card.app.clone(), card.summary.id.clone()));
        self.changed.push(card.rect);
        let rows = card_parts(card.rect, &card.summary);
        if let Some((id, _)) = rows.controls.iter().find(|(_, rect)| rect.contains(point)) {
            self.press = Some((card.app, card.summary.id, id.clone()));
            self.owed = true;
            return Vec::new();
        }
        if rows.collapse.contains(point) {
            return vec![Ask::Collapse {
                app: card.app,
                activity: card.summary.id,
            }];
        }
        if rows.close.contains(point) {
            return vec![Ask::Close {
                app: card.app,
                activity: card.summary.id,
            }];
        }
        // Anywhere else on the card takes hold of it: the card follows the
        // pointer until it is let go.
        let at = card.rect;
        self.drag = Some(Drag {
            app: card.app,
            activity: card.summary.id,
            offset: Vec2::new(point.x - at.x0, point.y - at.y0),
            rect: at,
            moved: false,
        });
        self.owed = true;
        Vec::new()
    }

    fn release_at(&mut self, point: Point) -> Vec<Ask> {
        if let Some((app, activity, control)) = self.press.take() {
            if let Some(card) = self
                .layout
                .cards
                .iter()
                .find(|card| card.app == app && card.summary.id == activity)
            {
                self.changed.push(card.rect);
            }
            // A press that lifts off its control runs nothing, as a button
            // everywhere else on the desktop does not.
            let runs = self
                .layout
                .cards
                .iter()
                .find(|card| card.app == app && card.summary.id == activity)
                .and_then(|card| {
                    card_parts(card.rect, &card.summary)
                        .controls
                        .into_iter()
                        .find(|(id, _)| *id == control)
                })
                .is_some_and(|(_, rect)| rect.contains(point));
            self.owed = true;
            return if runs {
                vec![Ask::Control {
                    app,
                    activity,
                    control,
                }]
            } else {
                Vec::new()
            };
        }
        let Some(drag) = self.drag.take() else {
            return Vec::new();
        };
        if !drag.moved {
            self.owed = true;
            return Vec::new();
        }
        // Let go, the card settles onto the edge it was thrown near, kept
        // inside the work area with a strip the person can grab again.
        let work = self.placement.work;
        let settled = ui::floating_surface::snap_to_edges(drag.rect, work, EDGE_INSET);
        let settled = keep_inside(settled, work);
        self.owed = true;
        vec![Ask::Moved {
            app: drag.app,
            activity: drag.activity,
            at: Some(settled),
        }]
    }

    /// A key pressed while the layer holds the keyboard.
    pub fn key(&mut self, key: &ui::winit::keyboard::Key) -> Option<Ask> {
        use ui::winit::keyboard::{Key, NamedKey};
        let focused = self.focus.clone()?;
        let index = self
            .layout
            .cards
            .iter()
            .position(|card| card.app == focused.0 && card.summary.id == focused.1)?;
        match key {
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight | NamedKey::Tab) => {
                let count = self.layout.cards.len().max(1);
                let next = index.saturating_add(1).checked_rem(count)?;
                let card = self.layout.cards.get(next)?;
                self.focus = Some((card.app.clone(), card.summary.id.clone()));
                self.refocused(&[index, next]);
                self.owed = true;
                None
            }
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                let previous = index
                    .checked_sub(1)
                    .unwrap_or_else(|| self.layout.cards.len().saturating_sub(1));
                let card = self.layout.cards.get(previous)?;
                self.focus = Some((card.app.clone(), card.summary.id.clone()));
                self.refocused(&[index, previous]);
                self.owed = true;
                None
            }
            Key::Named(NamedKey::Escape) => {
                self.focus = None;
                self.refocused(&[index]);
                self.owed = true;
                Some(Ask::Focus(false))
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let card = self.layout.cards.get(index)?;
                let control = card.summary.first_control()?;
                Some(Ask::Control {
                    app: card.app.clone(),
                    activity: card.summary.id.clone(),
                    control: control.id.clone(),
                })
            }
            _ => None,
        }
    }

    /// The layer as assistive technology reads it: one group per card, named
    /// by the activity's title, with its reading and its controls.
    #[must_use]
    pub fn accessibility(&self) -> Option<AccessNode> {
        if self.layout.cards.is_empty() {
            return None;
        }
        let groups = self
            .layout
            .cards
            .iter()
            .map(|card| {
                let summary = &card.summary;
                let mut children = vec![
                    AccessNode::new(Role::StaticText)
                        .with_name(summary.title.clone())
                        .with_value(summary.value.clone()),
                ];
                for control in summary.action_controls() {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(control.label.clone())
                            .with_action_target(target(card, &control.id))
                            .with_state(AccessState::new().focusable(true)),
                    );
                }
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(crate::i18n::t("activity-collapse"))
                        .with_action_target(target(card, "collapse")),
                );
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(summary.close_control().map_or_else(
                            || crate::i18n::t("activity-close"),
                            |close| close.label.clone(),
                        ))
                        .with_action_target(target(card, "close")),
                );
                AccessNode::new(Role::Group)
                    .with_name(summary.title.clone())
                    .with_description(summary.value.clone())
                    .with_bounds(card.rect)
                    .with_children(children)
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_name(crate::i18n::t("activities-layer"))
                .with_children(groups),
        )
    }

    /// Press the control assistive technology names by `target`.
    pub fn activate(&self, target: &str) -> Option<Ask> {
        let mut parts = target.strip_prefix("activity:")?.split(':');
        let app = parts.next()?.to_owned();
        let activity = parts.next()?.to_owned();
        let control = parts.next()?.to_owned();
        Some(match control.as_str() {
            "collapse" => Ask::Collapse { app, activity },
            "close" => Ask::Close { app, activity },
            _ => Ask::Control {
                app,
                activity,
                control,
            },
        })
    }

    /// What the layer shows, for a test that reads it without a screen.
    #[must_use]
    pub fn test_rects(&self) -> Vec<(String, Rect)> {
        self.layout
            .cards
            .iter()
            .map(|card| (format!("activity:{}", card.summary.id), self.dragged(card)))
            .collect()
    }
}

/// What the layer shows, for the host's hit testing, focus and assistive
/// technology.
#[derive(Clone, Debug, Default)]
pub struct Shown {
    /// Each card on screen: its app, its activity and where it stands.
    pub cards: Vec<(String, String, Rect)>,
    /// Activities on a phone waiting in the pill for the person to expand.
    pub folded: usize,
    /// Keyboard focus is on a card.
    pub keyboard: bool,
    /// A card is being dragged.
    pub dragging: bool,
    /// What the last frame changed, shadows and all: the part of the surface
    /// a host presents.
    pub damage: Rect,
}

/// The accessibility target naming `control` on `card`.
fn target(card: &Placed, control: &str) -> String {
    format!("activity:{}:{}:{control}", card.app, card.summary.id)
}

fn origin_of(rect: Rect) -> Point {
    rect.origin()
}

/// A settled card kept inside the work area with [`EDGE_INSET`] to spare.
fn keep_inside(rect: Rect, work: Rect) -> Rect {
    let x = rect.x0.clamp(
        work.x0 + EDGE_INSET,
        (work.x1 - EDGE_INSET - rect.width()).max(work.x0 + EDGE_INSET),
    );
    let y = rect.y0.clamp(
        work.y0 + EDGE_INSET,
        (work.y1 - EDGE_INSET - rect.height()).max(work.y0 + EDGE_INSET),
    );
    Rect::from_origin_size((x, y), rect.size())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::floating_layer::card_size;
    use ui::live_activity::{ControlKind, Kind};
    use ui::widget::floating_surface::card_radius;

    fn placement() -> Placement {
        Placement {
            work: Rect::new(0.0, 32.0, 1280.0, 832.0),
            bar: Rect::new(0.0, 0.0, 1280.0, 32.0),
            banners: None,
            readout: None,
        }
    }

    fn timer() -> Summary {
        Summary::new(Kind::Timer, "Tea", "4:32")
            .with_id("tea")
            .with_progress(0.5)
            .with_control(ui::live_activity::Control::new(
                "toggle",
                "Pause",
                ControlKind::Primary,
            ))
            .with_control(ui::live_activity::Control::new(
                "close",
                "End",
                ControlKind::Close,
            ))
    }

    fn layer() -> FloatingLayer {
        let mut layer = FloatingLayer::default();
        layer.set(
            placement(),
            Corner::BottomRight,
            vec![Card::new("so.tilo.clocks", timer())],
        );
        layer
    }

    fn music(value: &str) -> Card {
        Card::new(
            "so.tilo.music",
            Summary::new(Kind::Music, "Album", value).with_id("music"),
        )
    }

    /// A card that only ticks repaints its own ground: the cards that did not
    /// change stay as the screen has them.
    #[test]
    fn a_ticking_card_repaints_only_itself() {
        let theme = Theme::default();
        let mut text = TextRenderer::new();
        let mut layer = FloatingLayer::default();
        layer.set(
            placement(),
            Corner::BottomRight,
            vec![Card::new("so.tilo.clocks", timer()), music("Track 1")],
        );
        layer.paint(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
        let cards = layer.layout().cards.clone();
        assert_eq!(cards.len(), 2);

        layer.set(
            placement(),
            Corner::BottomRight,
            vec![Card::new("so.tilo.clocks", timer()), music("Track 2")],
        );
        layer.paint(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
        assert_eq!(
            layer.damage(),
            painted_bounds(cards[1].rect, &theme),
            "the card that changed, shadows and all"
        );
    }

    /// Dragging a card repaints where it left and where it is, and no other
    /// card.
    #[test]
    fn a_dragged_card_repaints_where_it_was_and_where_it_is() {
        let theme = Theme::default();
        let mut text = TextRenderer::new();
        let mut layer = FloatingLayer::default();
        layer.set(
            placement(),
            Corner::BottomRight,
            vec![Card::new("so.tilo.clocks", timer()), music("Track 1")],
        );
        layer.paint(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
        let cards = layer.layout().cards.clone();
        let grabbed = cards[0].rect;
        let point = Point::new(grabbed.x0 + 8.0, grabbed.y0 + 8.0);
        assert!(layer.pointer_button(point, true).is_empty());
        let moved = Point::new(point.x - 60.0, point.y + 40.0);
        assert!(layer.pointer_motion(moved));
        layer.paint(&mut Scene::new(), Affine::IDENTITY, &mut text, &theme);
        let damage = layer.damage();
        assert_eq!(
            damage,
            painted_bounds(grabbed, &theme).union(painted_bounds(
                Rect::from_origin_size(
                    Point::new(grabbed.x0 - 60.0, grabbed.y0 + 40.0),
                    grabbed.size()
                ),
                &theme
            )),
            "the card's old and new ground"
        );
    }

    /// A press on a control runs it when it lifts on that control, and runs
    /// nothing when the pointer moves off it.
    #[test]
    fn a_control_runs_when_the_press_lifts_on_it() {
        let mut layer = layer();
        let card = layer.layout().cards.first().expect("a card").rect;
        let rows = card_parts(card, &timer());
        let (id, control) = rows.controls.first().expect("a control");
        assert_eq!(id, "toggle");
        let at = control.center();
        assert!(layer.pointer_button(at, true).is_empty());
        assert_eq!(
            layer.pointer_button(at, false),
            vec![Ask::Control {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
                control: "toggle".into(),
            }]
        );
        assert!(layer.pointer_button(at, true).is_empty());
        assert!(
            layer
                .pointer_button(Point::new(at.x + control.width(), at.y), false)
                .is_empty(),
            "lifted off the control: nothing runs"
        );
    }

    /// The header's close and collapse ask the session, which owns what they
    /// mean.
    #[test]
    fn the_header_asks_to_close_or_collapse() {
        let mut layer = layer();
        let card = layer.layout().cards.first().expect("a card").rect;
        let rows = card_parts(card, &timer());
        assert_eq!(
            layer.pointer_button(rows.close.center(), true),
            vec![Ask::Close {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
            }]
        );
        assert_eq!(
            layer.pointer_button(rows.collapse.center(), true),
            vec![Ask::Collapse {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
            }]
        );
    }

    /// Dragging a card loose and letting it go snaps it to the edge it was
    /// thrown near and tells the session where it is.
    #[test]
    fn a_dragged_card_settles_on_the_edge_and_reports_where() {
        let mut layer = layer();
        let card = layer.layout().cards.first().expect("a card").rect;
        let grab = Point::new(card.x0 + 40.0, card.y0 + 6.0);
        assert!(layer.pointer_button(grab, true).is_empty());
        assert!(layer.pointer_motion(Point::new(grab.x - 900.0, grab.y - 300.0)));
        let asks = layer.pointer_button(Point::new(grab.x - 900.0, grab.y - 300.0), false);
        let Some(Ask::Moved {
            app,
            activity,
            at: Some(at),
        }) = asks.first()
        else {
            panic!("a moved card reports its place: {asks:?}");
        };
        assert_eq!(app, "so.tilo.clocks");
        assert_eq!(activity, "tea");
        assert!(
            (at.x0 - (placement().work.x0 + EDGE_INSET)).abs() < 0.5,
            "it settles onto the left edge: {at:?}"
        );
        assert!(at.width() > 0.0 && at.height() > 0.0);
    }

    /// Space runs the first control of the card the keyboard is on, and
    /// Escape leaves the layer.
    #[test]
    fn the_keyboard_walks_the_cards_and_runs_their_first_control() {
        use ui::winit::keyboard::{Key, NamedKey};
        let mut layer = layer();
        let card = layer.layout().cards.first().expect("a card").rect;
        layer.pointer_button(Point::new(card.x0 + 40.0, card.y0 + 2.0), true);
        layer.pointer_button(Point::new(card.x0 + 40.0, card.y0 + 2.0), false);
        assert!(layer.shown().keyboard);
        assert_eq!(
            layer.key(&Key::Named(NamedKey::Space)),
            Some(Ask::Control {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
                control: "toggle".into(),
            })
        );
        assert_eq!(
            layer.key(&Key::Named(NamedKey::Escape)),
            Some(Ask::Focus(false))
        );
        assert!(!layer.shown().keyboard);
    }

    /// What a screen reader reads: one group per card with its own controls.
    #[test]
    fn each_card_is_one_group_named_by_its_title() {
        let layer = layer();
        let tree = layer.accessibility().expect("a layer with a card");
        assert_eq!(tree.children.len(), 1);
        let group = tree.children.first().expect("the card's group");
        assert_eq!(group.name.as_deref(), Some("Tea"));
        let targets: Vec<&str> = group
            .children
            .iter()
            .filter_map(|child| child.action_target.as_deref())
            .collect();
        assert_eq!(
            targets,
            [
                "activity:so.tilo.clocks:tea:toggle",
                "activity:so.tilo.clocks:tea:collapse",
                "activity:so.tilo.clocks:tea:close"
            ]
        );
        assert_eq!(
            layer.activate("activity:so.tilo.clocks:tea:toggle"),
            Some(Ask::Control {
                app: "so.tilo.clocks".into(),
                activity: "tea".into(),
                control: "toggle".into(),
            })
        );
    }

    /// Only the cards' shapes take the pointer: a press beside one reaches
    /// what is under it.
    #[test]
    fn the_shadow_lets_the_pointer_through() {
        let layer = layer();
        let card = layer.layout().cards.first().expect("a card").rect;
        assert!(layer.accepts(card.center()));
        assert!(!layer.accepts(Point::new(card.x1 + 12.0, card.y1 + 12.0)));
        assert!(!layer.accepts(Point::new(20.0, 20.0)));
    }

    /// A card the layout holds draws a height that covers every row it has.
    #[test]
    fn a_cards_height_holds_all_its_rows() {
        let summary = timer();
        let size = card_size(placement().work, &summary);
        let rect = Rect::from_origin_size((100.0, 100.0), size);
        let rows = card_parts(rect, &summary);
        assert!(rows.controls.first().expect("a control").1.y1 <= rect.y1);
        assert!(rows.close.y1 <= rect.y1);
        assert!(rows.value.expect("a reading").y0 >= rows.title.y1 - 0.5);
        assert!(card_radius(rect, &Theme::dark()) > 0.0);
    }
}
