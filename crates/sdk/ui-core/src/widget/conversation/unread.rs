//! Where a reader's unread lines start.
//!
//! A conversation the reader comes back to marks the first line they had not
//! read when they opened it, with a rule and "New messages" over it: the
//! place to start reading again, kept while they read on, so lines arriving
//! meanwhile do not move it. The app decides which line that is (from its
//! read marks); the face is the same in every conversation.

use vello::kurbo::Rect;

use crate::component::Component;

use crate::chrome::nav_row_height;
use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// The "New messages" divider: a rule across the column, its label at the
/// trailing end, a list row tall like a day divider
/// ([`super::DayDividerFace`]).
pub struct UnreadDividerFace;

impl UnreadDividerFace {
    /// The height a divider takes: one list row, at every text scale.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        nav_row_height(theme)
    }

    /// What the divider says.
    #[must_use]
    pub fn label() -> String {
        super::t("conversation-new-messages")
    }

    /// Paint the divider across `band`, its rule running from `leading` to
    /// the label, which ends at `trailing` (the message column's edges).
    pub fn paint(paint: &mut PaintCtx<'_>, band: Rect, leading: f64, trailing: f64) {
        let theme = paint.theme;
        let label = Self::label();
        let style = theme.text(TextRole::Caption);
        let gap = f64::from(theme.tokens.spacing.s3);
        let room = crate::num::f32_of_f64((trailing - leading).max(0.0));
        let shown = paint.text.elide_to_width_style(&label, room, &style);
        let width = f64::from(paint.text.measure_style(&shown, None, &style).0);
        let label_x = trailing - width;
        if label_x - gap > leading {
            crate::surface::separator(
                paint.scene,
                paint.transform,
                leading,
                label_x - gap,
                band.center().y,
                0.0,
                theme.tokens.colors.surfaces.content,
            );
        }
        paint.text_role_in_band(
            &shown,
            TextRole::Caption,
            theme.text_primary,
            label_x,
            band,
            None,
        );
    }
}

/// The divider as a widget, for a list built of widgets: a band across its column with its
/// label at the trailing end, over the first line the reader had not read.
///
/// A described component, so a host that validates what an app sends builds it: a list told
/// where the person stopped reading (`VirtualListProps::unseen_from`) stands one over the row
/// of that item, and the host knows it from the toolkit's catalogue as it knows every other
/// component.
#[derive(Component, Default)]
#[component(name = "UnreadDivider", role = "Group", label = "name")]
pub struct UnreadDividerProps {
    /// What it is called, for assistive technology; empty for the toolkit's own word.
    pub name: Option<String>,
}

/// [`UnreadDividerProps`], built: an unread divider over a row.
pub struct UnreadDivider {
    rect: Rect,
    /// It was drawn, and shows the same since: a divider never changes, so it repaints only
    /// where it moved.
    drawn: bool,
    name: String,
}

impl UnreadDivider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            drawn: false,
            name: String::new(),
        }
    }

    /// The height a divider takes, in `theme`: the same as the day divider's band.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        UnreadDividerFace::height(theme)
    }
}

impl Default for UnreadDivider {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::component::Described for UnreadDivider {
    type Props = UnreadDividerProps;

    fn from_props(_key: &str, props: UnreadDividerProps) -> Self {
        Self {
            rect: Rect::ZERO,
            drawn: false,
            name: props.name.unwrap_or_default(),
        }
    }

    /// The name it is called changes where the divider stands.
    fn set_props(&mut self, props: UnreadDividerProps) -> bool {
        let name = props.name.unwrap_or_default();
        if self.name != name {
            self.name = name;
            self.drawn = false;
        }
        true
    }
}

impl crate::widget::Widget for UnreadDivider {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt as _;
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::style_helpers::length(crate::num::f32_of_f64(Self::height(theme))),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        UnreadDividerFace::paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            self.rect.x0,
            self.rect.x1,
        );
    }

    fn name(&self) -> &'static str {
        "UnreadDivider"
    }

    fn needs_redraw(&self) -> bool {
        !self.drawn
    }

    fn mark_drawn(&mut self) {
        self.drawn = true;
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let name = if self.name.is_empty() {
            UnreadDividerFace::label()
        } else {
            self.name.clone()
        };
        Some(AccessNode::new(Role::Heading { level: 3 }).with_name(name))
    }
}
