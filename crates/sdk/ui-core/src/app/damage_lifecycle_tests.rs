use super::*;
use crate::widget::TaffyTreeExt;
use std::collections::HashMap;

struct DrawUpdatedDamage {
    damage: KurboRect,
    dirty: bool,
}

impl Widget for DrawUpdatedDamage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.damage = KurboRect::new(80.0, 0.0, 100.0, 8.0);
    }

    fn name(&self) -> &'static str {
        "DrawUpdatedDamage"
    }

    fn get_rect(&self) -> KurboRect {
        self.damage
    }

    crate::redraw_flag!();

    fn collect_damage(&self, regions: &mut Vec<KurboRect>) {
        regions.push(self.damage);
    }
}

#[test]
fn submitted_damage_uses_draw_updated_geometry_before_dirty_clear() {
    let mut widget = DrawUpdatedDamage {
        damage: KurboRect::new(0.0, 0.0, 20.0, 8.0),
        dirty: true,
    };
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    widget.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::default(),
    );

    let mut regions = Vec::new();
    collect_submitted_widget_damage(&widget, &mut regions);
    assert_eq!(regions, vec![KurboRect::new(80.0, 0.0, 100.0, 8.0)]);
    assert!(widget.dirty, "collection must not consume the dirty flag");

    widget.mark_drawn();
    assert!(!widget.dirty);
}

/// A leaf whose box the test can shift without rebuilding the tree, the way
/// scrolling or a reflowed sibling moves a retained widget.
struct Shiftable {
    shift: std::rc::Rc<std::cell::Cell<f64>>,
    rect: KurboRect,
}

impl Widget for Shiftable {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = taffy::Style {
            size: taffy::Size::from_lengths(120.0, 40.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x) + self.shift.get();
        let y = origin.y + f64::from(layout.location.y);
        self.rect = KurboRect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
    }

    fn name(&self) -> &'static str {
        "Shiftable"
    }

    fn get_rect(&self) -> KurboRect {
        self.rect
    }
}

/// A relayout repaints the whole window only when something moved: an
/// in-place change (typing into a fixed-size field) keeps to its damage,
/// while moved geometry or a new viewport cannot be described by it.
#[test]
fn relayout_redraws_everything_only_when_geometry_moves() {
    let shift = std::rc::Rc::new(std::cell::Cell::new(0.0));
    let mut app = App::new(
        State::new(),
        Box::new(Shiftable {
            shift: shift.clone(),
            rect: KurboRect::ZERO,
        }),
    );
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    let (rebuilt, ..) = run_layout_pass(&mut app, root, 400, 300, 1.0, true, None).unwrap();
    assert!(rebuilt, "a rebuilt tree repaints everything");

    app.layout.layout_dirty = true;
    let (moved, ..) = run_layout_pass(&mut app, root, 400, 300, 1.0, false, None).unwrap();
    assert!(
        !moved,
        "a relayout that moves nothing keeps to widget damage"
    );

    shift.set(24.0);
    app.layout.layout_dirty = true;
    let (moved, ..) = run_layout_pass(&mut app, root, 400, 300, 1.0, false, None).unwrap();
    assert!(moved, "a widget that moved leaves its old pixels behind");

    let (resized, ..) = run_layout_pass(&mut app, root, 500, 300, 1.0, false, None).unwrap();
    assert!(resized, "a new viewport repaints everything");

    let (idle, ..) = run_layout_pass(&mut app, root, 500, 300, 1.0, false, None).unwrap();
    assert!(!idle, "no layout work, no full repaint");
}

// ── Collaborators' presence ─────────────────────────────────────────────
//
// A visible peer used to turn widget damage off: every frame repainted the
// window. The presence layer now damages the marks that changed, and these
// tests hold it to the contract that matters: no pixel outside the
// submitted damage differs between the frame on screen and the next.

const PRESENCE_SCALE: f64 = 2.0;
const PRESENCE_WINDOW: KurboRect = KurboRect::new(0.0, 0.0, 640.0, 400.0);
const LOCAL_FIELD: KurboRect = KurboRect::new(40.0, 40.0, 160.0, 72.0);

fn presence_app(theme: &Theme) -> App {
    let mut app = App::new(
        State::new(),
        Box::new(DrawUpdatedDamage {
            damage: LOCAL_FIELD,
            dirty: false,
        }),
    );
    app.theme = theme.clone();
    app
}

fn physical_window() -> KurboRect {
    PRESENCE_WINDOW.scale_from_origin(PRESENCE_SCALE)
}

/// One frame of the window's presence through the production path: draw the
/// layer, then turn the widgets' damage (`widget`, logical) and the layer's
/// into what the frame submits. `None` repaints the window.
fn presence_frame(
    app: &mut App,
    layer: &mut crate::presence::PresenceLayer,
    widget: &[KurboRect],
) -> Option<Vec<KurboRect>> {
    let presence = draw_presence_layer(
        app,
        layer,
        Affine::scale(PRESENCE_SCALE),
        PRESENCE_WINDOW,
        crate::time::now(),
    )?;
    let mut regions = widget.to_vec();
    submit_presence_damage(
        &mut regions,
        &presence,
        layer.widgets_place_content_marks(),
        PRESENCE_SCALE,
        physical_window(),
    )
    .then_some(regions)
}

/// Assert that presenting `next` with `damage` over `shown` leaves no stale
/// pixel: everything outside the damage is identical in both frames.
/// Returns how many pixels inside the damage changed.
fn assert_damage_covers_change(
    theme: &Theme,
    shown: &Scene,
    next: &Scene,
    damage: &[KurboRect],
) -> usize {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the test window is a small fixed size"
    )]
    let (width, height) = (
        physical_window().width() as u32,
        physical_window().height() as u32,
    );
    let mut renderer = crate::offscreen::OffscreenRenderer::new(width, height);
    let ground = theme.tokens.colors.surfaces.content;
    let before = renderer.render(shown, ground);
    let after = renderer.render(next, ground);
    let mut repaired = 0_usize;
    for y in 0..height {
        for x in 0..width {
            let was = crate::offscreen::OffscreenRenderer::sample(&before, width, x, y);
            let is = crate::offscreen::OffscreenRenderer::sample(&after, width, x, y);
            if was == is {
                continue;
            }
            let centre = Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            assert!(
                damage.iter().any(|rect| rect.contains(centre)),
                "pixel ({x}, {y}) changed outside the submitted damage {damage:?}: stale presence"
            );
            repaired = repaired.saturating_add(1);
        }
    }
    repaired
}

#[test]
fn remote_cursor_move_damages_only_old_and_new_cursor() {
    for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
        let mut app = presence_app(&theme);
        let mut layer = crate::presence::PresenceLayer::new();
        app.presence_ctx
            .upsert_remote_cursor("alice".into(), [120.0, 150.0], Some("Alice"));

        assert_eq!(
            presence_frame(&mut app, &mut layer, &[]),
            None,
            "the first frame has nothing on screen to compare with"
        );
        let mut shown = layer.scene().clone();
        layer.presented();

        // A peer whose pointer rests over the window costs nothing.
        assert_eq!(
            presence_frame(&mut app, &mut layer, &[]),
            Some(Vec::new()),
            "a still pointer damages nothing"
        );
        layer.presented();

        app.presence_ctx
            .upsert_remote_cursor("alice".into(), [220.0, 150.0], Some("Alice"));
        let moved = presence_frame(&mut app, &mut layer, &[])
            .expect("a pointer move keeps to widget damage");
        assert!(!moved.is_empty(), "a moved pointer must be repainted");
        let window = physical_window();
        let area: f64 = moved.iter().map(KurboRect::area).sum();
        assert!(
            area < window.area() / 6.0,
            "a pointer move damaged {area} of {} px: {moved:?}",
            window.area()
        );
        for rect in &moved {
            assert!(
                rect.width() <= 220.0 * PRESENCE_SCALE && rect.height() <= 120.0 * PRESENCE_SCALE,
                "{rect:?} is not one pointer or its name"
            );
        }
        let next = layer.scene().clone();
        let repaired = assert_damage_covers_change(&theme, &shown, &next, &moved);
        assert!(repaired > 0, "the pointer must actually have moved");
        layer.presented();
        shown = next;

        // The peer's pointer leaves the window: where it was is erased.
        app.presence_ctx.remove_remote_cursor("alice");
        let left = presence_frame(&mut app, &mut layer, &[])
            .expect("a pointer leaving keeps to widget damage");
        let next = layer.scene().clone();
        let erased = assert_damage_covers_change(&theme, &shown, &next, &left);
        assert!(erased > 0, "the old pointer must actually be erased");
        layer.presented();
        assert_eq!(presence_frame(&mut app, &mut layer, &[]), Some(Vec::new()));
    }
}

#[test]
fn idle_peer_keeps_local_frames_to_widget_damage() {
    let theme = Theme::dark();
    let alone = {
        let mut app = presence_app(&theme);
        let mut layer = crate::presence::PresenceLayer::new();
        assert_eq!(presence_frame(&mut app, &mut layer, &[]), None);
        layer.presented();
        presence_frame(&mut app, &mut layer, &[LOCAL_FIELD]).expect("no peer, no repaint")
    };

    let mut app = presence_app(&theme);
    let mut layer = crate::presence::PresenceLayer::new();
    app.presence_ctx
        .upsert_remote_cursor("alice".into(), [300.0, 200.0], Some("Alice"));
    assert_eq!(presence_frame(&mut app, &mut layer, &[]), None);
    layer.presented();
    // A local keystroke dirties one field; the peer is where she was.
    let typed = presence_frame(&mut app, &mut layer, &[LOCAL_FIELD])
        .expect("an idle peer must not force a full repaint");
    assert_eq!(
        typed, alone,
        "with an idle peer a keystroke shades what it shades alone"
    );
}

#[test]
fn remote_caret_keeps_pointer_frames_bounded_and_repaints_local_edits() {
    let theme = Theme::light();
    let mut app = presence_app(&theme);
    let mut layer = crate::presence::PresenceLayer::new();
    let key = crate::presence::PresenceKey::widget(&[crate::presence::PathSegment::Field(
        "title".into(),
    )])
    .expect("a widget key");
    app.presence_ctx
        .set_remote_caret("alice".into(), key.clone(), 3);
    app.presence_ctx
        .upsert_remote_cursor("alice".into(), [300.0, 200.0], Some("Alice"));
    assert_eq!(presence_frame(&mut app, &mut layer, &[]), None);
    layer.presented();

    // Her pointer moving changes no widget: only the pointer repaints.
    app.presence_ctx
        .upsert_remote_cursor("alice".into(), [310.0, 200.0], Some("Alice"));
    let pointer = presence_frame(&mut app, &mut layer, &[])
        .expect("a pointer move beside a remote caret keeps to its marks");
    assert!(!pointer.is_empty());
    layer.presented();

    // A local edit can move her caret and its name chip, which hangs
    // outside the field and is in no widget's damage: repaint.
    assert_eq!(presence_frame(&mut app, &mut layer, &[LOCAL_FIELD]), None);
    layer.presented();

    // Her caret moving changes what the field paints: repaint, once.
    app.presence_ctx.set_remote_caret("alice".into(), key, 4);
    assert_eq!(presence_frame(&mut app, &mut layer, &[]), None);
    layer.presented();
    assert_eq!(presence_frame(&mut app, &mut layer, &[]), Some(Vec::new()));
}
