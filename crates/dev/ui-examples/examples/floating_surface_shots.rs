//! Production render of the shared soft floating-window chrome.

use std::collections::HashMap;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use tilo_ui::ColorCompat;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::{TextRenderer, TextWeight};
use tilo_ui::theme::Theme;
use tilo_ui::widget::{FloatingWindow, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

#[derive(Clone, Copy)]
enum PreviewKind {
    Call,
    Navigation,
}

struct Preview {
    kind: PreviewKind,
    rect: Rect,
}

impl Preview {
    fn new(kind: PreviewKind) -> Self {
        Self {
            kind,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for Preview {
    #[allow(
        clippy::expect_used,
        reason = "a fresh visual-harness layout tree cannot reject its first leaf"
    )]
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let n = t
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .expect("fresh preview leaf");
        map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
        n
    }

    fn apply_layout(&mut self, t: &TaffyTree<()>, n: NodeId, origin: Point) {
        let l = t.layout(n).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x,
            origin.y,
            origin.x + f64::from(l.size.width),
            origin.y + f64::from(l.size.height),
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the visual-harness painter keeps its deterministic paint order explicit"
    )]
    fn draw(
        &mut self,
        s: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        match self.kind {
            PreviewKind::Navigation => {
                s.fill(Fill::NonZero, t, c.neutral[1], None, &self.rect);
                for x in (0..6).map(|i| self.rect.x0 + 28.0 + f64::from(i) * 78.0) {
                    s.stroke(
                        &Stroke::new(18.0),
                        t,
                        c.neutral[3].with_alpha_factor(0.65),
                        None,
                        &vello::kurbo::Line::new((x, self.rect.y0), (x - 54.0, self.rect.y1)),
                    );
                }
                let mut route = BezPath::new();
                route.move_to((self.rect.x0 + 48.0, self.rect.y1 - 18.0));
                route.curve_to(
                    (self.rect.x0 + 108.0, self.rect.y1 - 92.0),
                    (self.rect.x0 + 210.0, self.rect.y1 - 60.0),
                    (self.rect.x1 - 44.0, self.rect.y0 + 38.0),
                );
                s.stroke(&Stroke::new(7.0), t, c.intent.inverted_hover, None, &route);
                let instruction = Rect::new(
                    self.rect.x0 + 14.0,
                    self.rect.y0 + 14.0,
                    self.rect.x1 - 14.0,
                    self.rect.y0 + 68.0,
                );
                s.fill(
                    Fill::NonZero,
                    t,
                    c.surfaces.raised.with_alpha_factor(0.94),
                    None,
                    &RoundedRect::from_rect(instruction, 14.0),
                );
                tr.draw_text_weighted(
                    s,
                    "↱  Turn right on Main Street",
                    theme.tokens.typography.scale.base,
                    c.text_primary,
                    t.pre_translate((instruction.x0 + 16.0, instruction.y0 + 17.0).into()),
                    None,
                    TextWeight::Semibold,
                );
                tr.draw_text(
                    s,
                    "300 m  ·  12 min  ·  4.2 km",
                    theme.tokens.typography.scale.xs,
                    c.text_dim,
                    t.pre_translate((instruction.x0 + 43.0, instruction.y0 + 36.0).into()),
                    None,
                );
            }
            PreviewKind::Call => {
                s.fill(Fill::NonZero, t, c.neutral[1], None, &self.rect);
                let avatar = Circle::new(
                    (
                        self.rect.center().x,
                        self.rect.y0 + self.rect.height() * 0.43,
                    ),
                    46.0,
                );
                s.fill(Fill::NonZero, t, c.presence.get(1), None, &avatar);
                tr.draw_text_weighted(
                    s,
                    "AM",
                    22.0,
                    vello::peniko::Color::WHITE,
                    t.pre_translate((avatar.center.x - 18.0, avatar.center.y - 13.0).into()),
                    None,
                    TextWeight::Semibold,
                );
                tr.draw_text_weighted(
                    s,
                    "Avery Morgan",
                    theme.tokens.typography.scale.base,
                    c.text_primary,
                    t.pre_translate((self.rect.center().x - 55.0, avatar.center.y + 62.0).into()),
                    None,
                    TextWeight::Medium,
                );
                let chip = Rect::new(
                    self.rect.center().x - 38.0,
                    self.rect.y1 - 45.0,
                    self.rect.center().x + 38.0,
                    self.rect.y1 - 16.0,
                );
                s.fill(
                    Fill::NonZero,
                    t,
                    c.semantic.danger,
                    None,
                    &RoundedRect::from_rect(chip, 14.0),
                );
                tr.draw_text_weighted(
                    s,
                    "End call",
                    theme.tokens.typography.scale.xs,
                    vello::peniko::Color::WHITE,
                    t.pre_translate((chip.x0 + 14.0, chip.y0 + 7.0).into()),
                    None,
                    TextWeight::Semibold,
                );
            }
        }
    }

    fn name(&self) -> &'static str {
        "FloatingSurfacePreview"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn stamp(window: &mut dyn Widget, theme: &Theme, tr: &mut TextRenderer, scene: &mut Scene) {
    let mut taffy = TaffyTree::<()>::new();
    let mut map = HashMap::new();
    let node = window.build_layout(&mut taffy, theme, &mut map);
    let _ = taffy.compute_layout(
        node,
        Size {
            width: AvailableSpace::Definite(1280.0),
            height: AvailableSpace::Definite(720.0),
        },
    );
    window.apply_layout(&taffy, node, Point::ZERO);
    window.draw(scene, Affine::IDENTITY, tr, theme, &PresenceCtx::default());
}

pub fn run(_: &[String]) {
    let light = std::env::var("TILO_SHOTS_THEME").is_ok_and(|v| v == "light");
    let theme = if light { Theme::light() } else { Theme::dark() }.with_reduce_motion(true);
    let out = tilo_ui::env::var("TILO_UI_SNAPSHOT_OUT")
        .unwrap_or_else(|_| "/tmp/tilo-floating-surfaces.png".into());
    let mut renderer = OffscreenRenderer::new(1280, 720);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let c = &theme.tokens.colors;

    for (i, (x0, x1, title)) in [
        (24.0, 500.0, "Chat · Product room"),
        (522.0, 1256.0, "The desktop continues →"),
    ]
    .into_iter()
    .enumerate()
    {
        let card = Rect::new(x0, 66.0, x1, 682.0);
        s_panel(&mut scene, &theme, card);
        tr.draw_text_weighted(
            &mut scene,
            title,
            theme.tokens.typography.scale.base,
            c.text_dim,
            Affine::translate((x0 + 22.0, 84.0)),
            None,
            TextWeight::Medium,
        );
        if i == 1 {
            for j in 0..4 {
                let block = Rect::new(
                    x0 + 26.0 + f64::from(j) * 164.0,
                    126.0,
                    x0 + 166.0 + f64::from(j) * 164.0,
                    632.0,
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    c.overlay_subtle,
                    None,
                    &RoundedRect::from_rect(block, 16.0),
                );
            }
        }
    }

    let mut call = FloatingWindow::new("Call · 06:42", Preview::new(PreviewKind::Call))
        .open(true)
        .with_initial_size(330.0, 330.0);
    call.layout_within(Rect::new(58.0, 126.0, 466.0, 650.0));
    stamp(&mut call, &theme, &mut tr, &mut scene);

    let mut nav = FloatingWindow::new("Navigation · 12 min", Preview::new(PreviewKind::Navigation))
        .open(true)
        .with_initial_size(480.0, 320.0);
    nav.layout_within(Rect::new(690.0, 222.0, 1230.0, 642.0));
    stamp(&mut nav, &theme, &mut tr, &mut scene);

    renderer.render_png(&scene, c.surfaces.base, out);
}

fn s_panel(scene: &mut Scene, theme: &Theme, rect: Rect) {
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.raised,
        None,
        &RoundedRect::from_rect(rect, 22.0),
    );
}
