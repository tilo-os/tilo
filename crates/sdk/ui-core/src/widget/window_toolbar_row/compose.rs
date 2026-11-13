//! A window bar's row as a page shows it ([`crate::composition`]): each slot the face it is painted
//! with (its icon or words on the bar's button ground, in its state), the location as its words,
//! and a compact window's notice as its icon and text. Keyed by the targets the row's semantics
//! name the slots by ([`ToolbarRow::accessibility`]).
use super::{
    ACTION_TARGET_PREFIX, BACK_TARGET, FORWARD_TARGET, Icon, LEADING_TARGET_PREFIX,
    LOCATION_ICON_PX, NOTICE_TARGET_PREFIX, OVERFLOW_TARGET, Rect, SIDEBAR_TARGET, Slot, TextRole,
    Theme, ToolbarRow, VIEW_TARGET_PREFIX, VIEWS_MENU_TARGET,
};
use crate::composition::{Face, Own, TextLook};
use crate::widget::window_controls::{ButtonFace, icon_button_face};

/// Where the row's location stands in its composition (it has no target of
/// its own unless it can be edited or picked from).
pub(crate) const LOCATION_KEY: &str = "location";
pub(crate) const STRIP_KEY: &str = "strip-notice";

impl ToolbarRow {
    pub(crate) fn composed_slots(&self, theme: &Theme) -> Vec<(String, Rect, Own)> {
        let mut out = Vec::new();
        let Some(toolbar) = self.toolbar() else {
            return out;
        };
        let slots = &self.slots;
        let state = |slot: Slot, enabled: bool, on: bool| ButtonFace {
            hovered: self.hover.as_ref() == Some(&slot)
                || (self.menu.is_open() && self.menu_slot.as_ref() == Some(&slot)),
            pressed: self.pressed.as_ref() == Some(&slot),
            on,
            enabled,
            destructive: false,
            primary: false,
        };
        let mut icon = |target: String, rect: Rect, glyph: Icon, face: ButtonFace| {
            if rect.area() > 0.0
                && let Some(face) = icon_button_face(rect, glyph, face).composed(theme)
            {
                out.push((target, rect, Own::Face(face)));
            }
        };
        if toolbar.sidebar.is_some() && !self.slots.sidebar_folded(toolbar) {
            icon(
                SIDEBAR_TARGET.into(),
                slots.sidebar,
                Icon::PanelLeft,
                state(
                    Slot::Sidebar,
                    !toolbar.sidebar_unavailable,
                    toolbar.sidebar == Some(true),
                ),
            );
        }
        if let Some(nav) = toolbar.navigation {
            icon(
                BACK_TARGET.into(),
                slots.back,
                Icon::ArrowLeft,
                state(Slot::Back, nav.back, false),
            );
            if !slots.forward_folded(toolbar) {
                icon(
                    FORWARD_TARGET.into(),
                    slots.forward,
                    Icon::ArrowRight,
                    state(Slot::Forward, nav.forward, false),
                );
            }
        }
        let labelled = |rect: Rect, name: &str, primary: bool, face: ButtonFace| {
            super::labelled_face(rect, name, primary)
                .hovered(face.hovered && face.enabled)
                .pressed(face.pressed && face.enabled)
                .toggled(face.on)
                .disabled(!face.enabled)
                .composed(theme)
        };
        let mut faces = Vec::new();
        for (i, (action, rect)) in toolbar.leading.iter().zip(&slots.leading).enumerate() {
            let face = state(Slot::Leading(i), action.enabled, action.on);
            let target = format!("{LEADING_TARGET_PREFIX}{}", action.id);
            if action.labelled && slots.labelled {
                faces.push((
                    target,
                    *rect,
                    labelled(*rect, &action.name, action.primary, face),
                ));
            } else if let Some(glyph) = Icon::from_name(&action.icon) {
                let face = ButtonFace {
                    primary: action.primary,
                    ..face
                };
                faces.push((
                    target,
                    *rect,
                    icon_button_face(*rect, glyph, face).composed(theme),
                ));
            }
        }
        for (i, (action, rect)) in toolbar.actions.iter().zip(&slots.actions).enumerate() {
            let face = ButtonFace {
                destructive: action.destructive,
                primary: action.primary,
                ..state(Slot::Action(i), action.enabled, action.on)
            };
            let target = format!("{ACTION_TARGET_PREFIX}{}", action.id);
            if action.labelled && slots.actions_labelled {
                faces.push((
                    target,
                    *rect,
                    labelled(*rect, &action.name, action.primary, face),
                ));
            } else if let Some(glyph) = Icon::from_name(&action.icon) {
                faces.push((
                    target,
                    *rect,
                    icon_button_face(*rect, glyph, face).composed(theme),
                ));
            }
        }
        if let Some(notice) = toolbar.notice.as_ref() {
            let strip = self.strip.buttons();
            let rects = if strip.is_empty() {
                &slots.notice
            } else {
                &strip
            };
            for (i, (action, rect)) in notice.actions.iter().zip(rects).enumerate() {
                let face = state(Slot::Notice(i), action.enabled, false);
                let target = format!("{NOTICE_TARGET_PREFIX}{}", action.id);
                let words =
                    !strip.is_empty() || slots.notice_labelled.get(i).copied().unwrap_or(false);
                if words {
                    faces.push((
                        target,
                        *rect,
                        labelled(*rect, &action.name, action.primary, face),
                    ));
                } else if let Some(glyph) = Icon::from_name(&action.icon) {
                    faces.push((
                        target,
                        *rect,
                        icon_button_face(*rect, glyph, face).composed(theme),
                    ));
                }
            }
        }
        if let Some(views) = toolbar.views.as_ref() {
            for (view, rect) in views.choices.iter().zip(&slots.views) {
                let chosen = view.id == views.chosen;
                let face = crate::widget::ButtonFace::new(*rect, &view.label)
                    .role(crate::widget::ButtonRole::Plain)
                    .control_size(crate::widget::ControlSize::Small)
                    .toggled(chosen);
                faces.push((
                    format!("{VIEW_TARGET_PREFIX}{}", view.id),
                    *rect,
                    face.composed(theme),
                ));
            }
        }
        out.extend(
            faces
                .into_iter()
                .filter(|(_, rect, _)| rect.area() > 0.0)
                .filter_map(|(target, rect, face)| Some((target, rect, Own::Face(face?)))),
        );
        if slots.overflow.area() > 0.0 {
            let face = state(Slot::Overflow, true, false);
            if let Some(face) =
                icon_button_face(slots.overflow, Icon::MoreHorizontal, face).composed(theme)
            {
                out.push((OVERFLOW_TARGET.into(), slots.overflow, Own::Face(face)));
            }
        }
        if slots.views_menu.area() > 0.0
            && let Some(views) = toolbar.views.as_ref()
        {
            let chosen = views.chosen_index().and_then(|i| views.choices.get(i));
            let label = chosen.map_or(views.name.as_str(), |view| view.label.as_str());
            if let Some(mut face) = crate::widget::ButtonFace::new(slots.views_menu, label)
                .role(crate::widget::ButtonRole::Plain)
                .control_size(crate::widget::ControlSize::Small)
                .composed(theme)
            {
                face.trailing =
                    Icon::ChevronDown.glyph(LOCATION_ICON_PX, theme.tokens.colors.text_dim, false);
                out.push((VIEWS_MENU_TARGET.into(), slots.views_menu, Own::Face(face)));
            }
        }
        if let Some(location) = toolbar.location.as_ref()
            && slots.location.area() > 0.0
            && self.field.is_none()
            && !slots.views_in_location()
        {
            let (shown, ink) = super::paint::location_shown(location, slots.form, theme);
            let notice = toolbar.notice.as_ref().map(|notice| notice.kind);
            let glyph = location
                .icon
                .as_deref()
                .and_then(Icon::from_name)
                .and_then(|glyph| {
                    let color = notice.map_or(theme.tokens.colors.text_dim, |kind| {
                        crate::widget::toast::kind_color(kind, theme)
                    });
                    glyph.glyph(LOCATION_ICON_PX, color, false)
                });
            let style = theme.text(TextRole::Body);
            let picker = !location.editable && !location.choices.is_empty();
            out.push((
                LOCATION_KEY.into(),
                slots.location,
                Own::Face(Face {
                    icon: glyph,
                    label: (!shown.is_empty()).then(|| {
                        TextLook::new(shown.to_owned(), &style, Some(ink)).with_max_lines(Some(1))
                    }),
                    detail: None,
                    trailing: picker
                        .then(|| {
                            Icon::ChevronDown.glyph(
                                LOCATION_ICON_PX,
                                theme.tokens.colors.text_dim,
                                false,
                            )
                        })
                        .flatten(),
                    fill: None,
                    edge: None,
                    radius: f64::from(theme.tokens.radii.control),
                    pad: if picker {
                        f64::from(theme.tokens.spacing.s2)
                    } else {
                        0.0
                    },
                    gap: f64::from(theme.tokens.spacing.s2),
                    spread: true,
                }),
            ));
        }
        if let Some((notice, text, _, _)) = self.strip_notice() {
            let style = theme.text(TextRole::Body);
            out.push((
                STRIP_KEY.into(),
                text,
                Own::Face(Face {
                    icon: crate::widget::toast::kind_icon(notice.kind).glyph(
                        LOCATION_ICON_PX,
                        crate::widget::toast::kind_color(notice.kind, theme),
                        false,
                    ),
                    label: Some(
                        TextLook::new(
                            notice.text.clone(),
                            &style,
                            Some(theme.tokens.colors.text_primary),
                        )
                        .with_max_lines(Some(2)),
                    ),
                    detail: None,
                    trailing: None,
                    fill: None,
                    edge: None,
                    radius: 0.0,
                    pad: 0.0,
                    gap: f64::from(theme.tokens.spacing.s2),
                    spread: true,
                }),
            ));
        }
        out
    }

    pub(crate) fn strip_band(&self) -> Option<Rect> {
        self.strip.notice()?;
        let rect = self.strip.rect();
        (rect.area() > 0.0).then_some(rect)
    }
}
