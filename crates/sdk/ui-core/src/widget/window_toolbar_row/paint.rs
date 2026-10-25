//! How a window bar's row draws: its slots, the location, and the
//! pickers.

use super::slot_change::{Pose, SlotKey};
use super::{
    Affine, Breadcrumb, ButtonFace, FieldFace, Icon, LOCATION_ICON_PX, LocationLabel, Point,
    PresenceCtx, Rect, Scene, SegmentedFace, Slot, Slots, TextRenderer, Theme, ToggleGroup,
    ToolbarRow, VIEWS_SIZE, Widget, WindowToolbar, paint_icon_button,
};

impl ToolbarRow {
    /// Paint the row's menus, in the window's top layer.
    pub(crate) fn paint_popups(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(&mut self.menu, scene, transform, tr, theme, presence);
        if self.suggestions_open() {
            self.suggestions.paint(scene, transform, tr, theme);
        }
        self.paint_picker(scene, transform, (tr, theme, presence));
    }

    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.measure_views(theme);
        let now = crate::time::now();
        self.start_slot_change(theme);
        if let Some((outgoing, opacity)) = self.fade.outgoing(now)
            && outgoing.is_some()
            && let Some((toolbar, slots)) = self.outgoing.as_ref()
        {
            layered(scene, transform, self.row, opacity, |scene| {
                let _ = paint_slots(
                    scene,
                    transform,
                    (tr, theme, presence),
                    toolbar,
                    slots,
                    (None, None),
                );
            });
        } else if !self.fade.is_moving(now) {
            self.outgoing = None;
        }
        let incoming = self.fade.incoming(now);
        let Some((_, toolbar)) = self.shown.as_ref() else {
            return;
        };
        let interaction = Interaction {
            poses: self.slot_changes.poses(crate::motion::frame_now()),
            hover: self.hover.clone(),
            pressed: self.pressed.clone(),
            editing: self.field.is_some(),
            menu_open: self
                .menu
                .is_open()
                .then(|| self.menu_slot.clone())
                .flatten(),
        };
        let row = self.row;
        let slots = &self.slots;
        let field = self.field.as_mut();
        let crumbs = self.crumbs.as_mut().map(|(_, crumbs)| crumbs);
        let mut trigger = None;
        layered(scene, transform, row, incoming, |scene| {
            trigger = paint_slots(
                scene,
                transform,
                (tr, theme, presence),
                toolbar,
                slots,
                (Some(&interaction), crumbs),
            );
            if let Some(field) = field {
                Widget::draw(field, scene, transform, tr, theme, presence);
            }
        });
        if let Some(trigger) = trigger {
            self.choice_trigger = trigger;
        }
        self.strip.paint(
            scene,
            transform,
            (tr, theme),
            (
                match interaction.hover {
                    Some(Slot::Notice(i)) => Some(i),
                    _ => None,
                },
                match interaction.pressed {
                    Some(Slot::Notice(i)) => Some(i),
                    _ => None,
                },
            ),
        );
        self.paint_leaving(scene, transform, (tr, theme, presence), incoming);
        self.paint_reactions(scene, transform, (tr, theme));
    }

    /// Start moving the slots the app changed since the last paint, now
    /// that they are laid out and measured where they go.
    pub(super) fn start_slot_change(&mut self, theme: &Theme) {
        let now = crate::motion::frame_now();
        if let Some(before) = self.changed_from.take()
            && let Some((_, toolbar)) = self.shown.as_ref()
        {
            self.slot_changes.start(
                std::rc::Rc::new(before),
                (toolbar, &self.slots),
                theme.motion(),
                now,
            );
        }
        self.slot_changes.prune(now);
    }

    /// The slots the app took away, fading out where they stood: each
    /// painted from the row it stood in, cut to its place.
    fn paint_leaving(
        &self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
        incoming: f32,
    ) {
        let now = crate::motion::frame_now();
        for leaving in self.slot_changes.leaving() {
            let opacity = leaving.opacity(now) * incoming;
            if opacity <= 0.0 {
                continue;
            }
            let at = transform.pre_translate(leaving.offset);
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                opacity,
                at,
                &leaving.rect.inflate(2.0, 2.0),
            );
            let (toolbar, slots) = leaving.from.as_ref();
            let _ = paint_slots(
                scene,
                at,
                (tr, theme, presence),
                toolbar,
                slots,
                (None, None),
            );
            scene.pop_layer();
        }
    }
}

/// What the person is doing with the row, for its paint.
pub(super) struct Interaction {
    /// The slots arriving and moving after the app changed them, by who
    /// they are; empty at rest.
    pub(super) poses: std::collections::HashMap<SlotKey, Pose>,
    pub(super) hover: Option<Slot>,
    pub(super) pressed: Option<Slot>,
    pub(super) editing: bool,
    /// The button whose menu is open (the location, for its picker): it
    /// stays lifted.
    pub(super) menu_open: Option<Slot>,
}

/// Paint `paint` at `opacity`, in a layer only while it is part-way.
pub(super) fn layered(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    opacity: f32,
    paint: impl FnOnce(&mut Scene),
) {
    if opacity <= 0.0 {
        return;
    }
    let part = opacity < 1.0;
    if part {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &clip.inflate(2.0, 2.0),
        );
    }
    paint(scene);
    if part {
        scene.pop_layer();
    }
}

/// Whether `slot` is drawn on: the sidebar's toggle while the sidebar is
/// open, a toggle while it is on (a panel's, while the panel is open).
pub(super) fn shows_on(toolbar: &WindowToolbar, slot: &Slot) -> bool {
    match slot {
        Slot::Sidebar => toolbar.sidebar == Some(true),
        Slot::Leading(i) => toolbar.leading.get(*i).is_some_and(|action| action.on),
        Slot::Action(i) => toolbar.actions.get(*i).is_some_and(|action| action.on),
        _ => false,
    }
}

/// The slots in motion of a row's paint, by who they are (`None`: at rest).
type Poses<'a> = Option<&'a std::collections::HashMap<SlotKey, Pose>>;

/// Paint one slot as it is posed: at its opacity and moved by its offset
/// while it arrives or glides ([`super::slot_change`]), else as it stands.
fn posed(
    scene: &mut Scene,
    transform: Affine,
    (poses, toolbar): (Poses<'_>, &WindowToolbar),
    (slot, rect): (&Slot, Rect),
    paint: impl FnOnce(&mut Scene, Affine),
) {
    let Some((opacity, offset)) = poses.and_then(|poses| poses.get(&SlotKey::of(toolbar, slot)))
    else {
        paint(scene, transform);
        return;
    };
    let at = transform.pre_translate(*offset);
    layered(scene, at, rect, *opacity, |scene| paint(scene, at));
}

/// Paint `toolbar`'s slots. Returns where a location with choices painted
/// its picker, which its menu hangs from.
pub(super) fn paint_slots(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    toolbar: &WindowToolbar,
    slots: &Slots,
    (interaction, crumbs): (Option<&Interaction>, Option<&mut Breadcrumb>),
) -> Option<Rect> {
    let face = |slot: Slot, enabled: bool, on: bool| ButtonFace {
        hovered: interaction.is_some_and(|i| i.hover.as_ref() == Some(&slot)),
        pressed: interaction.is_some_and(|i| i.pressed.as_ref() == Some(&slot)),
        on,
        enabled,
        destructive: false,
    };
    let poses: Poses<'_> = interaction
        .map(|i| &i.poses)
        .filter(|poses| !poses.is_empty());
    let pose = (poses, toolbar);
    if toolbar.sidebar.is_some() {
        let lifted = face(
            Slot::Sidebar,
            !toolbar.sidebar_unavailable,
            shows_on(toolbar, &Slot::Sidebar),
        );
        posed(
            scene,
            transform,
            pose,
            (&Slot::Sidebar, slots.sidebar),
            |scene, at| {
                paint_icon_button(
                    scene,
                    at,
                    tr,
                    theme,
                    (slots.sidebar, Icon::PanelLeft),
                    lifted,
                );
            },
        );
    }
    if let Some(nav) = toolbar.navigation {
        let back = face(Slot::Back, nav.back, false);
        posed(
            scene,
            transform,
            pose,
            (&Slot::Back, slots.back),
            |scene, at| {
                paint_icon_button(scene, at, tr, theme, (slots.back, Icon::ArrowLeft), back);
            },
        );
        let forward = face(Slot::Forward, nav.forward, false);
        posed(
            scene,
            transform,
            pose,
            (&Slot::Forward, slots.forward),
            |scene, at| {
                paint_icon_button(
                    scene,
                    at,
                    tr,
                    theme,
                    (slots.forward, Icon::ArrowRight),
                    forward,
                );
            },
        );
    }
    for (i, (action, rect)) in toolbar.leading.iter().zip(&slots.leading).enumerate() {
        let slot = Slot::Leading(i);
        let lifted = face(slot.clone(), action.enabled, shows_on(toolbar, &slot));
        if action.labelled && slots.labelled && rect.area() > 0.0 {
            let face = labelled_face(*rect, &action.name)
                .hovered(lifted.hovered && lifted.enabled)
                .pressed(lifted.pressed && lifted.enabled)
                .toggled(lifted.on)
                .disabled(!lifted.enabled);
            posed(scene, transform, pose, (&slot, *rect), |scene, at| {
                crate::widget::Button::paint(scene, at, tr, theme, &face);
            });
            continue;
        }
        let Some(icon) = Icon::from_name(&action.icon).filter(|_| rect.area() > 0.0) else {
            continue;
        };
        posed(scene, transform, pose, (&slot, *rect), |scene, at| {
            paint_icon_button(scene, at, tr, theme, (*rect, icon), lifted);
        });
    }
    if let Some(views) = toolbar.views.as_ref()
        && let (Some(first), Some(last)) = (slots.views.first(), slots.views.last())
    {
        let labels: Vec<&str> = views.choices.iter().map(|v| v.label.as_str()).collect();
        let hovered = interaction.and_then(|i| match &i.hover {
            Some(Slot::View(index)) => Some(*index),
            _ => None,
        });
        let face = SegmentedFace::new(
            &slots.views,
            &labels,
            views
                .chosen_index()
                .unwrap_or(crate::widget::toggle_group::NONE_CHOSEN),
        )
        // Switching the window's view is tabs, the look of every view
        // switcher (`ViewTabs`), never a segmented control.
        .style(crate::widget::toggle_group::SegmentedStyle::Tabs)
        .control_size(VIEWS_SIZE)
        .hovered(hovered);
        posed(
            scene,
            transform,
            pose,
            (&Slot::View(0), first.union(*last)),
            |scene, at| ToggleGroup::paint(scene, at, tr, theme, &face),
        );
    }
    if let Some(views) = toolbar.views.as_ref()
        && slots.views_menu.area() > 0.0
    {
        let open = interaction.is_some_and(|i| i.menu_open == Some(Slot::ViewsMenu));
        let lift = LocationLift {
            hovered: interaction.is_some_and(|i| i.hover == Some(Slot::ViewsMenu)),
            pressed: open || interaction.is_some_and(|i| i.pressed == Some(Slot::ViewsMenu)),
            focused: false,
        };
        // As the views' picker the location names its period, not the view.
        let picker = match toolbar.location.as_ref() {
            Some(location) if slots.views_in_location() => {
                let (shown, ink) = location_shown(location, slots.form, theme);
                PickerText::new(shown, ink).edited(location.edited)
            }
            _ => {
                let chosen = views.chosen_index().and_then(|i| views.choices.get(i));
                let label = chosen.map_or(views.name.as_str(), |view| view.label.as_str());
                PickerText::new(label, theme.tokens.colors.text_primary)
            }
        };
        posed(
            scene,
            transform,
            pose,
            (&Slot::ViewsMenu, slots.views_menu),
            |scene, at| {
                paint_picker(scene, at, (tr, theme), picker, slots.views_menu, lift);
            },
        );
    }
    for (i, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
        let Some(icon) = Icon::from_name(&action.icon) else {
            continue;
        };
        let slot = Slot::Action(i);
        let mut lifted = face(slot.clone(), action.enabled, shows_on(toolbar, &slot));
        lifted.hovered |= interaction.is_some_and(|it| it.menu_open == Some(Slot::Action(i)));
        lifted.destructive = action.destructive;
        posed(scene, transform, pose, (&slot, *rect), |scene, at| {
            paint_icon_button(scene, at, tr, theme, (*rect, icon), lifted);
        });
    }
    if slots.overflow.area() > 0.0 {
        let open = interaction.is_some_and(|i| i.menu_open == Some(Slot::Overflow));
        let mut overflow = face(Slot::Overflow, true, false);
        overflow.hovered |= open;
        posed(
            scene,
            transform,
            pose,
            (&Slot::Overflow, slots.overflow),
            |scene, at| {
                paint_icon_button(
                    scene,
                    at,
                    tr,
                    theme,
                    (slots.overflow, Icon::MoreHorizontal),
                    overflow,
                );
            },
        );
    }
    let actions = toolbar
        .notice
        .as_ref()
        .map_or(&[][..], |notice| notice.actions.as_slice());
    for (i, (action, rect)) in actions.iter().zip(&slots.notice).enumerate() {
        let slot = Slot::Notice(i);
        let lifted = face(slot.clone(), action.enabled, false);
        if slots.notice_labelled.get(i).copied().unwrap_or(false) {
            let face = labelled_face(*rect, &action.name)
                .hovered(lifted.hovered && lifted.enabled)
                .pressed(lifted.pressed && lifted.enabled)
                .disabled(!lifted.enabled);
            posed(scene, transform, pose, (&slot, *rect), |scene, at| {
                crate::widget::Button::paint(scene, at, tr, theme, &face);
            });
        } else if let Some(icon) = Icon::from_name(&action.icon) {
            posed(scene, transform, pose, (&slot, *rect), |scene, at| {
                paint_icon_button(scene, at, tr, theme, (*rect, icon), lifted);
            });
        }
    }
    let editing = interaction.is_some_and(|i| i.editing);
    let location = toolbar.location.as_ref()?;
    if slots.location.area() <= 0.0 || editing || slots.views_in_location() {
        return None;
    }
    let lifted = LocationLift {
        hovered: interaction.is_some_and(|i| i.hover == Some(Slot::Location)),
        pressed: interaction.is_some_and(|i| {
            i.pressed == Some(Slot::Location) || i.menu_open == Some(Slot::Location)
        }),
        focused: false,
    };
    let mut trigger = None;
    posed(
        scene,
        transform,
        pose,
        (&Slot::Location, slots.location),
        |scene, at| {
            trigger = paint_location(
                scene,
                at,
                (tr, theme, presence),
                (location, crumbs),
                (slots.location, slots.form),
                (lifted, toolbar.notice.as_ref().map(|notice| notice.kind)),
            );
        },
    );
    trigger
}

/// How the pointer stands on a picker: the location, or the views menu.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LocationLift {
    pub(crate) hovered: bool,
    /// Pressed, or its menu is open.
    pub(crate) pressed: bool,
    /// Holds keyboard focus with the ring shown.
    pub(crate) focused: bool,
}

/// The location at rest: its icon, then its label in form `form`
/// ([`Slots::form`]), elided at the end. An
/// editable one stands in a field's frame, so it reads as one to click; one
/// with choices reads as a picker, its label and a chevron on the bar's
/// plain button face, and returns where it painted that.
pub(super) fn paint_location(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    (location, crumbs): (
        &crate::window_toolbar::ToolbarLocation,
        Option<&mut Breadcrumb>,
    ),
    (rect, form): (Rect, usize),
    (lift, notice): (LocationLift, Option<crate::widget::toast::ToastKind>),
) -> Option<Rect> {
    if !location.editable && !location.choices.is_empty() {
        let (shown, ink) = location_shown(location, form, theme);
        return Some(paint_picker(
            scene,
            transform,
            (tr, theme),
            PickerText::new(shown, ink).edited(location.edited),
            rect,
            lift,
        ));
    }
    let mut text_rect = rect;
    if location.editable {
        let face = FieldFace::new(rect, "").hovered(lift.hovered);
        face.paint_chrome(scene, transform, theme);
        text_rect = face.text_rect(theme);
    }
    if let Some(icon) = location.icon.as_deref().and_then(Icon::from_name) {
        let c = Point::new(text_rect.x0 + LOCATION_ICON_PX / 2.0, rect.center().y);
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(c, (LOCATION_ICON_PX, LOCATION_ICON_PX)),
            icon,
            // A notice's icon says its kind in its kind's colour.
            notice.map_or(theme.tokens.colors.text_dim, |kind| {
                crate::widget::toast::kind_color(kind, theme)
            }),
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_rect.x0 += LOCATION_ICON_PX + f64::from(theme.tokens.spacing.s2);
    }
    // A location with levels shows them as a breadcrumb.
    if let Some(crumbs) = crumbs
        && !location.segments.is_empty()
    {
        if text_rect.width() > 0.0 {
            crumbs.set_rect(Rect::new(text_rect.x0, rect.y0, text_rect.x1, rect.y1));
            Widget::draw(crumbs, scene, transform, tr, theme, presence);
        }
        return None;
    }
    let (shown, ink) = location_shown(location, form, theme);
    if shown.is_empty() || text_rect.width() <= 0.0 {
        return None;
    }
    let label = LocationLabel::measure(tr, theme, shown, location.edited, text_rect.width());
    let y = rect.center().y - label.height / 2.0;
    label.paint(scene, transform, (tr, theme), (text_rect.x0, y), ink);
    None
}

/// What the location says, in its ink: form `form` of its label
/// ([`crate::window_toolbar::ToolbarLocation::form`]), else its
/// placeholder.
pub(crate) fn location_shown<'a>(
    location: &'a crate::window_toolbar::ToolbarLocation,
    form: usize,
    theme: &Theme,
) -> (&'a str, vello::peniko::Color) {
    if location.label.is_empty() {
        return (
            location.placeholder.as_str(),
            theme.tokens.colors.text_tertiary,
        );
    }
    (location.form(form), theme.tokens.colors.text_primary)
}

/// A labelled leading action's text button in `rect`, saying `name`
/// ([`crate::window_toolbar::ToolbarAction::labelled`]): the bar's plain
/// face, at the size of its icon buttons.
pub(super) fn labelled_face(rect: Rect, name: &str) -> crate::widget::ButtonFace<'_> {
    crate::widget::ButtonFace::new(rect, name)
        .role(crate::widget::ButtonRole::Plain)
        .control_size(crate::widget::ControlSize::Small)
}

/// How wide a picker ([`paint_picker`]) whose label is `label_w` wide
/// stands.
pub(crate) fn picker_width(label_w: f64, theme: &Theme) -> f64 {
    let pad = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s1);
    pad * 2.0 + label_w.ceil() + gap + LOCATION_ICON_PX
}

/// What a picker ([`paint_picker`]) says: its label in its ink, and
/// whether the document it names has unsaved edits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PickerText<'a> {
    shown: &'a str,
    ink: vello::peniko::Color,
    edited: bool,
}

impl<'a> PickerText<'a> {
    pub(crate) fn new(shown: &'a str, ink: vello::peniko::Color) -> Self {
        Self {
            shown,
            ink,
            edited: false,
        }
    }

    /// The document it names has unsaved edits: the edited word follows.
    pub(crate) fn edited(mut self, edited: bool) -> Self {
        self.edited = edited;
        self
    }
}

/// A label that opens a menu of choices (a location with choices, the
/// folded views): the label (elided at the end) and a trailing chevron, on
/// the plain button face the bar's buttons lift with, as wide as they need
/// and never wider than `rect`. Returns the face's rect, which the menu
/// hangs from.
pub(crate) fn paint_picker(
    scene: &mut Scene,
    transform: Affine,
    (tr, theme): (&mut TextRenderer, &Theme),
    text: PickerText<'_>,
    rect: Rect,
    lift: LocationLift,
) -> Rect {
    let pad = f64::from(theme.tokens.spacing.s2);
    let gap = f64::from(theme.tokens.spacing.s1);
    let room = (rect.width() - pad * 2.0 - gap - LOCATION_ICON_PX).max(0.0);
    let label = LocationLabel::measure(tr, theme, text.shown, text.edited, room);
    let label_w = label.width.min(room);
    let face_rect = Rect::new(
        rect.x0,
        rect.y0,
        (rect.x0 + pad * 2.0 + label_w + gap + LOCATION_ICON_PX).min(rect.x1),
        rect.y1,
    );
    let face = crate::widget::ButtonFace::new(face_rect, "")
        .role(crate::widget::ButtonRole::Plain)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(lift.hovered)
        .pressed(lift.pressed)
        .focused(lift.focused);
    crate::widget::Button::paint(scene, transform, tr, theme, &face);
    let y = rect.center().y - label.height / 2.0;
    label.paint(
        scene,
        transform,
        (tr, theme),
        (face_rect.x0 + pad, y),
        text.ink,
    );
    let chevron_x = face_rect.x0 + pad + label_w + gap;
    crate::icons::draw_icon(
        scene,
        transform,
        Rect::new(
            chevron_x,
            rect.center().y - LOCATION_ICON_PX / 2.0,
            chevron_x + LOCATION_ICON_PX,
            rect.center().y + LOCATION_ICON_PX / 2.0,
        ),
        Icon::ChevronDown,
        theme.tokens.colors.text_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    face_rect
}
