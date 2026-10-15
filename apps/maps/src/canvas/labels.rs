//! Screen-space typography over the offline vector geometry. Labels remain
//! measured at the active interface scale rather than stretching with map zoom.

use super::{LabelPlacer, MapView, pin_bounds, place_label_rect};
use crate::draw;
use crate::model::MapModel;
use crate::sample::{Basemap, LabelKind, MapLabel};
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use vello::kurbo::{Point, Rect, Vec2};

struct PlacedLabel<'a> {
    label: &'a MapLabel,
    plate: Rect,
    badge_width: f64,
}

fn layout<'a>(
    ctx: &mut PaintCtx<'_>,
    view: MapView<'_>,
    base: &'a Basemap,
    model: &MapModel,
) -> Vec<PlacedLabel<'a>> {
    let gap = f64::from(ctx.theme.tokens.spacing.s1);
    let mut placement = LabelPlacer {
        occupied: model
            .places
            .iter()
            .map(|place| {
                pin_bounds(
                    view.proj.project(place.pos),
                    model.selected.as_deref() == Some(&place.id),
                    view.pal.marker_scale,
                )
            })
            .collect(),
        gap,
    };
    if let Some(place) = model.selected_place() {
        placement.occupied.push(place_label_rect(
            ctx,
            view.proj.project(place.pos),
            &place.name,
        ));
    }
    let mut placed = Vec::new();
    for label in &base.labels {
        // Overview labels have their own hierarchy: retain stations, water
        // and districts when zooming out, not a cloud of tiny street names.
        if view.proj.zoom() < 13.5 && matches!(label.kind, LabelKind::Street | LabelKind::Park) {
            continue;
        }
        let style = ctx.theme.text(TextRole::Caption);
        let style = if matches!(label.kind, LabelKind::Station(_) | LabelKind::Neighborhood) {
            style.strong()
        } else {
            style
        };
        let pad = f64::from(ctx.theme.tokens.spacing.s2);
        let badge_width = match label.kind {
            LabelKind::Station(text) => draw::measure_w(ctx.text, text, &style) + 2.0 * pad,
            _ => 0.0,
        };
        let width = draw::measure_w(ctx.text, label.name, &style) + 2.0 * pad + badge_width;
        let height = draw::line_h(ctx.text, &style) + 2.0 * gap;
        let anchor = view.proj.project(label.pos);
        let radius = if badge_width > 0.0 {
            12.0 * view.pal.marker_scale
        } else {
            0.0
        };
        let preferred = if badge_width > 0.0 {
            Rect::from_origin_size(
                (anchor.x + radius + 2.0 * gap, anchor.y + radius + gap),
                (width, height),
            )
        } else {
            Rect::from_center_size(anchor, (width, height))
        };
        // A name may move one line up/down or part of its width sideways,
        // but never clip under Directions or cover a higher-priority name.
        let offsets = [
            Vec2::ZERO,
            Vec2::new(0.0, -height - gap),
            Vec2::new(-width * 0.4, 0.0),
            Vec2::new(width * 0.4, 0.0),
            Vec2::new(0.0, -2.0 * (height + gap)),
            Vec2::new(width * 0.4, height + gap),
            Vec2::new(-width * 0.4, -height - gap),
            Vec2::new(-width * 0.8, 0.0),
            Vec2::new(width * 0.8, 0.0),
        ];
        if let Some(plate) = offsets
            .into_iter()
            .find_map(|offset| placement.place(view, preferred + offset, anchor + offset, radius))
        {
            placed.push(PlacedLabel {
                label,
                plate,
                badge_width,
            });
        }
    }
    placed
}

pub fn draw_labels(ctx: &mut PaintCtx<'_>, view: MapView<'_>, base: &Basemap, model: &MapModel) {
    for PlacedLabel {
        label,
        plate,
        badge_width,
    } in layout(ctx, view, base, model)
    {
        let style = ctx.theme.text(TextRole::Caption);
        let style = if matches!(label.kind, LabelKind::Station(_) | LabelKind::Neighborhood) {
            style.strong()
        } else {
            style
        };
        let pad = f64::from(ctx.theme.tokens.spacing.s2);
        let radius = f64::from(ctx.theme.tokens.radii.detail);
        draw::fill_round(
            ctx.scene,
            ctx.transform,
            plate,
            radius,
            view.pal.label_plate,
        );
        let top = draw::vmid(ctx.text, plate.y0, plate.height(), &style);
        if let LabelKind::Station(text) = label.kind {
            let anchor = view.proj.project(label.pos);
            let attachment = Point::new(
                anchor.x.clamp(plate.x0, plate.x1),
                anchor.y.clamp(plate.y0, plate.y1),
            );
            draw::line(
                ctx.scene,
                ctx.transform,
                anchor,
                attachment,
                view.pal.marker_scale,
                draw::alpha(view.pal.route, 0.65),
            );
            draw::fill_circle(
                ctx.scene,
                ctx.transform,
                anchor,
                4.0 * view.pal.marker_scale,
                view.pal.marker_ring,
            );
            draw::fill_circle(
                ctx.scene,
                ctx.transform,
                anchor,
                2.5 * view.pal.marker_scale,
                view.pal.route,
            );
            let rect = Rect::new(plate.x0, plate.y0, plate.x0 + badge_width, plate.y1);
            draw::fill_round(ctx.scene, ctx.transform, rect, radius, view.pal.route);
            draw::text(
                ctx,
                text,
                Point::new(rect.x0 + pad, top),
                &style,
                view.pal.marker_ring,
            );
        }
        let ink = match label.kind {
            LabelKind::Neighborhood => draw::alpha(view.pal.label_ink, 0.72),
            LabelKind::Park => draw::mix(
                view.pal.label_ink,
                ctx.theme.tokens.colors.semantic.success,
                0.3,
            ),
            LabelKind::Water => draw::mix(
                view.pal.label_ink,
                ctx.theme.tokens.colors.semantic.info,
                0.3,
            ),
            _ => view.pal.label_ink,
        };
        draw::text(
            ctx,
            label.name,
            Point::new(plate.x0 + badge_width + pad, top),
            &style,
            ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{canvas::MapPalette, geo::Projection, sample};
    use ui::{TextRenderer, theme::Theme};
    use vello::{Scene, kurbo::Affine};

    #[test]
    fn names_are_measured_whole_and_decluttered_at_interface_scales() {
        let base = sample::basemap();
        let model = sample::default_model();
        for scale in [1.0, 1.5, 2.0] {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let theme = theme.with_ui_scale(scale);
                let pal = MapPalette::from_theme(&theme);
                let proj = Projection::new(model.center, 14.0, Point::new(900.0, 400.0));
                let clip = Rect::new(0.0, 0.0, 1440.0, 810.0);
                let chrome = [Rect::new(24.0, 24.0, 564.0, 786.0)];
                let view = MapView {
                    proj: &proj,
                    pal: &pal,
                    clip,
                    chrome: &chrome,
                };
                let mut scene = Scene::new();
                let mut text = TextRenderer::new();
                let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
                let placed = layout(&mut ctx, view, &base, &model);
                assert!(
                    placed
                        .iter()
                        .any(|p| matches!(p.label.kind, LabelKind::Station(_)))
                );
                assert!(placed.iter().any(|p| p.label.name == "Isar"));
                if scale == 1.5 {
                    for name in [
                        "Lindwurmstraße",
                        "Sendlinger Straße",
                        "Sonnenstraße",
                        "SENDLING",
                        "ALTSTADT",
                        "Goetheplatz",
                        "Sendlinger Tor",
                        "Marienplatz",
                        "Alter Südfriedhof",
                        "Theresienwiese",
                    ] {
                        assert!(
                            placed.iter().any(|p| p.label.name == name),
                            "missing visible {name}"
                        );
                    }
                }
                for (i, p) in placed.iter().enumerate() {
                    assert!(
                        view.shows_whole(p.plate),
                        "{} clips at {scale}",
                        p.label.name
                    );
                    assert!(
                        placed
                            .iter()
                            .skip(i + 1)
                            .all(|q| p.plate.intersect(q.plate).area() == 0.0)
                    );
                    if matches!(p.label.kind, LabelKind::Station(_)) {
                        assert!(p.badge_width > 0.0);
                    }
                }
                let blocked = MapView {
                    chrome: &[clip],
                    ..view
                };
                assert!(layout(&mut ctx, blocked, &base, &model).is_empty());
            }
        }
    }
}
