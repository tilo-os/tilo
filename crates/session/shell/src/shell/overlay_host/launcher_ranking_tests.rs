use super::*;
use std::cell::Cell;

struct RankingProvider {
    revision: Rc<Cell<u64>>,
    invoked: Rc<RefCell<Vec<String>>>,
}

impl crate::shell::search::SearchProvider for RankingProvider {
    fn id(&self) -> &'static str {
        "launcher-ranking-fixture"
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn search_revision(&self, _: &crate::shell::search::SearchContext<'_>) -> String {
        self.revision.get().to_string()
    }
    fn query(
        &self,
        _: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        let revision = self.revision.get();
        let rows = if revision == 0 {
            vec![("alpha", 90), ("beta", 80)]
        } else if revision == 1 {
            vec![("gamma", 100), ("alpha", 90), ("beta", 80)]
        } else {
            vec![("gamma", 100), ("alpha", 90)]
        };
        rows.into_iter()
            .map(|(id, score)| {
                let invoked = self.invoked.clone();
                crate::shell::search::SearchResult::nullary(
                    crate::shell::search::ResourceKind::Application,
                    self.id(),
                    id,
                    format!("Ranking fixture {id}"),
                    Some(format!("Available application · revision {revision}")),
                    score,
                    move |_, _, _| {
                        invoked.borrow_mut().push(format!("{id}:{revision}"));
                    },
                )
            })
            .collect()
    }
}

struct RankingHarness {
    h: AbsorbHarness,
    revision: Rc<Cell<u64>>,
    invoked: Rc<RefCell<Vec<String>>>,
}

fn ranking_fixture() -> RankingHarness {
    let mut h = build_harness_sized(1280.0, 800.0);
    let revision = Rc::new(Cell::new(0));
    let invoked = Rc::new(RefCell::new(Vec::new()));
    h.host.search_registry.register(Rc::new(RankingProvider {
        revision: revision.clone(),
        invoked: invoked.clone(),
    }));
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("ranking fixture");
    h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    assert_eq!(selected_label(&h), "Ranking fixture alpha");
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::ArrowDown))));
    assert_eq!(selected_label(&h), "Ranking fixture beta");
    RankingHarness {
        h,
        revision,
        invoked,
    }
}

fn selected_label(h: &AbsorbHarness) -> String {
    let selected = h.host.palette.selected_id().expect("a selected result");
    h.host
        .palette
        .visible_entries()
        .into_iter()
        .find(|entry| entry.id == selected)
        .expect("selected entry must exist")
        .label
        .clone()
}

#[test]
fn provider_revision_keeps_keyboard_choice_and_invokes_the_fresh_callback() {
    let mut f = ranking_fixture();
    f.revision.set(1);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut f.h, 1280.0, 800.0);
    assert_eq!(
        selected_label(&f.h),
        "Ranking fixture beta",
        "a new top result must not steal the user's choice"
    );
    assert_eq!(f.h.host.palette.query(), "ranking fixture");
    assert!(f.h.host.palette.is_focused());
    assert!(pump(&mut f.h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert_eq!(
        &*f.invoked.borrow(),
        &["beta:1"],
        "identity survives, but the callback must come from the current index"
    );
    assert!(!f.h.host.palette_visible());
}

#[test]
fn query_correction_and_removed_results_use_the_current_visible_list() {
    let mut f = ranking_fixture();
    f.revision.set(1);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    assert_eq!(selected_label(&f.h), "Ranking fixture beta");
    assert!(pump(
        &mut f.h,
        &key_pressed(Key::Named(NamedKey::Backspace))
    ));
    assert_eq!(f.h.host.palette.query(), "ranking fixtur");
    assert_eq!(
        selected_label(&f.h),
        "Ranking fixture gamma",
        "a new query starts at its own best match"
    );
    assert!(pump(&mut f.h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(f.invoked.borrow().is_empty());

    f.h.host.set_palette_visible(true);
    f.h.host.set_palette_focus(true);
    f.h.host.set_palette_query("ranking fixture");
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    let beta =
        f.h.host
            .palette
            .visible_entries()
            .iter()
            .position(|entry| entry.label == "Ranking fixture beta")
            .unwrap();
    f.h.host.palette.set_cursor(beta);
    f.revision.set(2);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    assert!(
        !f.h.host
            .palette
            .visible_entries()
            .iter()
            .any(|entry| entry.label == "Ranking fixture beta")
    );
    assert_eq!(selected_label(&f.h), "Ranking fixture gamma");
    assert!(pump(&mut f.h, &key_pressed(Key::Named(NamedKey::Enter))));
    assert_eq!(
        &*f.invoked.borrow(),
        &["gamma:2"],
        "a vanished result's old callback must never run"
    );
}

#[test]
fn ranked_selection_remains_pointer_reachable_after_resize() {
    let mut f = ranking_fixture();
    f.revision.set(1);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut f.h, 800.0, 600.0);
    let _ = paint_scene(&mut f.h, &Theme::dark());
    assert_eq!(selected_label(&f.h), "Ranking fixture beta");
    paint_then_click(&mut f.h, |host| {
        host.palette
            .row_rect(host.palette.cursor())
            .expect("selected row is painted")
            .center()
    });
    assert_eq!(&*f.invoked.borrow(), &["beta:1"]);
    assert!(!f.h.host.palette_visible());
}

#[test]
fn stable_ranking_production_render_matrix() {
    use ui::offscreen;
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/launcher-ranking-review");
    std::fs::create_dir_all(&out).expect("capture directory");
    let mut labels = Vec::new();
    for (name, width, height, theme) in [
        ("dark", 1280, 800, Theme::dark()),
        ("light", 1280, 800, Theme::light()),
        ("large-text", 800, 700, Theme::dark().with_text_scale(2.0)),
    ] {
        let mut f = ranking_fixture();
        f.revision.set(1);
        f.h.host
            .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
        f.h.host.force_palette_settled();
        for _ in 0..4 {
            f.h.host.take_structure_dirty();
            settle_with_theme(&mut f.h, f64::from(width), f64::from(height), &theme);
            if !f.h.host.structure_dirty() {
                break;
            }
        }
        assert!(!f.h.host.structure_dirty(), "capture must converge");
        assert!(f.h.host.palette_visible());
        labels.push(selected_label(&f.h));
        let scene = paint_scene(&mut f.h, &theme);
        let mut renderer = offscreen::OffscreenRenderer::new(width, height);
        let path = out.join(format!("{name}.png"));
        renderer.render_png(&scene, vello::peniko::Color::BLACK, &path);
        assert!(std::fs::metadata(path).expect("capture exists").len() > 1000);
    }
    assert_eq!(
        labels,
        vec!["Ranking fixture beta"; 3],
        "all captures must retain the chosen result after ranking changes"
    );
}
