//! The host's layer over the widget tree: members' carets, selections, typing
//! and writes, painted where each widget publishes geometry for them. A widget
//! never names a member; it says where a byte range or a box stands, and this
//! draws whoever is there.

use super::PresenceCtx;
use super::capability::{self, PublishesPresence};
use crate::draw::{DrawEnv, WidgetOverlay};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::capability::capability as capability_of;
use vello::Scene;
use vello::kurbo::Affine;

impl PresenceCtx {
    /// A draw environment whose overlay paints this presence's marks over
    /// every widget that asks for them.
    pub fn draw_env(&self) -> DrawEnv<'_> {
        let sites = self
            .here
            .iter()
            .map(|(key, peers)| {
                let faces = peers
                    .iter()
                    .map(|member| crate::draw::MemberFace {
                        name: self.display_name(member),
                        colour: self.color_for(member),
                        agent: !member.is_person(),
                        dimmed: self.availability_of(member).dims(),
                    })
                    .collect();
                (key.clone(), faces)
            })
            .collect();
        DrawEnv {
            viewport: self.viewport.get(),
            marks_changing: self.shows_writes(crate::time::now()),
            marks_generation: self.written_generation(),
            overlay: Some(self),
            sites: std::rc::Rc::new(sites),
            self_member: self.self_id().cloned(),
            presence: Some(self),
        }
    }

    /// Paint over `widget` what this presence shows there: the write that
    /// landed in its text, who is typing, their carets and selections, and the
    /// focus ring of whoever else is on it.
    fn paint_marks_over(
        &self,
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(key) = capability::presence_key(widget) else {
            return;
        };
        let now = crate::time::now();
        let published = capability_of::<dyn PublishesPresence>(widget);
        if let Some(published) = published {
            if let Some(view) = published.written_place(theme) {
                let mut place = view.place;
                self.draw_writes_in(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    (view.region, view.block, &view.text),
                    (view.edited, view.selected),
                    view.field,
                    &mut *place,
                );
            }
            if self.has_typing_marks() {
                for (user, range, alpha) in self.typing_marks_for_key(&key, now) {
                    let bands: Vec<(vello::kurbo::Rect, f32)> = published
                        .presence_range_rects(theme, &key, range)
                        .into_iter()
                        .map(|rect| (transform.transform_rect_bbox(rect), alpha))
                        .collect();
                    if !bands.is_empty() {
                        self.draw_peer_typing(scene, theme, &user, &bands);
                    }
                }
            }
            if let Some(list) = self.carets.get(&key) {
                for (user, caret) in list {
                    let name = self.display_name(user);
                    for rect in published.presence_range_rects(
                        theme,
                        &key,
                        super::SelectionRange {
                            start: *caret,
                            end: *caret,
                        },
                    ) {
                        self.draw_peer_caret_labeled(
                            scene,
                            theme,
                            text_renderer,
                            transform,
                            user,
                            &name,
                            rect,
                        );
                    }
                }
            }
            if let Some(list) = self.selections.get(&key) {
                for (user, range) in list {
                    for rect in published.presence_range_rects(theme, &key, *range) {
                        self.draw_peer_selection(scene, theme, transform, user, rect);
                    }
                }
            }
        }
        if let Some(user) = self.here.get(&key).and_then(|peers| peers.first()) {
            self.draw_peer_focus(scene, theme, user, widget.get_rect());
        }
    }
}

impl WidgetOverlay for PresenceCtx {
    fn paint_over(
        &self,
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.paint_marks_over(widget, scene, transform, text_renderer, theme);
    }
}
