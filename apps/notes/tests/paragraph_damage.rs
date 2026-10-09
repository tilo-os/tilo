//! Damage from the outer, ordinarily mounted component after real note input.
use crate::component_support::*;
use notes::{actions, doc::NotesDoc};
use std::{cell::RefCell, rc::Rc};
use ui::{Widget, headless::Driver};
use vello::{Scene, kurbo::Rect, peniko::Color};

struct DamageWitness {
    inner: Box<dyn Widget>,
    damage: Rc<RefCell<Vec<Rect>>>,
}
impl ui::widget::Decorator for DamageWitness {
    type Inner = dyn Widget;
    fn inner(&self) -> &Self::Inner {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        self.inner.as_mut()
    }
    fn mark_drawn(&mut self) {
        let mut damage = self.damage.borrow_mut();
        self.inner.collect_damage(&mut damage);
        self.inner.mark_drawn();
    }
}

#[test]
fn canonical_notes_paragraph_input_preserves_outer_component_sparse_gpu_pixels() {
    paragraph_input(true, "y", "xy", false);
}

#[test]
fn canonical_notes_default_motion_and_word_boundary_preserve_sparse_gpu_pixels() {
    paragraph_input(false, "y", "xy", false);
    paragraph_input(false, "Space", "x ", false);
}

#[test]
fn canonical_notes_timestamp_boundary_preserves_sparse_gpu_pixels() {
    pin_edit_clock(1_790_000_000);
    paragraph_input(false, "y", "xy", true);
}

fn paragraph_input(reduce_motion: bool, key: &str, prefix: &str, advance_timestamp: bool) {
    let f = NotebookFixture::rich();
    let body = (0..18)
        .map(|index| format!("Paragraph {index}: an ordinary document line."))
        .collect::<Vec<_>>()
        .join("\n\n");
    f.write_page(f.child, body.as_bytes());
    let checkout = notebook(&f);
    let damage = Rc::new(RefCell::new(Vec::new()));
    let witness = Rc::clone(&damage);
    let mut inputs = runner_inputs();
    inputs.theme.reduce_motion = reduce_motion;
    let background = inputs.theme.tokens.colors.surfaces.content;
    let mut driver = Driver::from_component_with_decorator(
        component(&f, 11, existing(&checkout)),
        (1200, 900),
        inputs,
        move |inner| {
            Box::new(DamageWitness {
                inner,
                damage: witness,
            })
        },
    )
    .unwrap();
    driver.set_platform_conventions(ui::PlatformConventions::new(ui::Platform::Linux));
    invoke(
        &mut driver,
        11,
        actions::page_resource_id(f.child),
        "open",
        None,
    );
    driver
        .perform_named("block_editor", &ui::a11y::Action::Focus)
        .unwrap();
    driver.key("Home");
    // The first edit can change the shared save indicator. The subsequent
    // ordinary edit proves stable app chrome forwards the paragraph damage.
    driver.key("x");
    let started = ui::time::now();
    let settled = driver.settle_for_capture(ui::time::Duration::from_secs(2));
    if !settled {
        eprintln!(
            "paragraph-settle-demand key={key:?} reduce_motion={reduce_motion} timestamp={advance_timestamp} elapsed={:?}",
            started.elapsed(),
        );
        report_unsettled_scene(&driver, &f.records, &checkout.id, background);
    }
    assert!(
        settled,
        "ordinary navigation and sidebar transitions settle before stable typing"
    );
    assert!(
        !driver.frame_schedule().needs_animation_frame,
        "the sparse typing baseline must have settled app chrome"
    );
    let before = driver.scene().clone();
    // A key interaction draws its press and release. Retain the union of
    // actual frame damage; the unchanged release must not erase the press.
    damage.borrow_mut().clear();
    type_with_timestamp(&mut driver, key, advance_timestamp);
    let actual = f
        .records
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, f.child)
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(
        actual[0],
        format!("{prefix}Paragraph 0: an ordinary document line.")
    );
    assert_eq!(actual.len(), 18);
    let damage = damage.borrow().clone();
    assert!(
        !damage.is_empty(),
        "the actual outer component records changed paint"
    );
    assert!(
        damage.iter().map(Rect::area).sum::<f64>() < 1200.0 * 900.0 / 2.0,
        "ordinary stable paragraph input must not damage the whole app: {damage:?}"
    );
    assert_sparse_pixels(&before, driver.scene(), &damage, background);
    close_driver(&mut driver);
}

fn assert_sparse_pixels(before: &Scene, after: &Scene, damage: &[Rect], background: Color) {
    let mut retained = ui::offscreen::OffscreenRenderer::new(1200, 900);
    let mut full = ui::offscreen::OffscreenRenderer::new(1200, 900);
    let old_pixels = retained.render(before, background);
    let damage = damage
        .iter()
        .map(|rect| rect.inflate(16.0, 16.0))
        .collect::<Vec<_>>();
    let work = retained.render_damage_regions_gpu_wait(after, background, &damage);
    assert!(
        work < 1200 * 900,
        "the outer component must exercise actual sparse rasterization"
    );
    let expected = full.render(after, background);
    assert_ne!(
        old_pixels, expected,
        "ordinary input must change visible note paint"
    );
    assert_eq!(
        retained.readback_current(),
        expected,
        "all retained app pixels match the full GPU oracle"
    );
}

fn report_unsettled_scene(
    driver: &Driver,
    records: &ui::record::RecordStore,
    record: &ui::record::RecordId,
    background: Color,
) {
    eprintln!(
        "paragraph-settle-state schedule={:?} requesters={:?} save={:?} images={}",
        driver.frame_schedule(),
        driver.frame_requesters(),
        records.save_status(record),
        ui::image_store::loads_in_flight(),
    );
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1200, 900);
    let pixels = renderer.render(driver.scene(), background);
    let png = ui::png::encode_rgba8(1200, 900, &pixels).unwrap();
    eprintln!(
        "paragraph-settle-failed-scene-png {}",
        serde_json::to_string(&png).unwrap()
    );
}

fn pin_edit_clock(seconds: u64) {
    ui::time::freeze_wall(
        ui::time::UNIX_EPOCH
            .checked_add(ui::time::Duration::from_secs(seconds))
            .unwrap(),
    );
}

fn date_captions(driver: &Driver) -> Vec<String> {
    driver
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| {
            node.label()
                .filter(|label| label.starts_with("Created "))
                .map(str::to_owned)
        })
        .collect()
}

fn type_with_timestamp(driver: &mut Driver, key: &str, advance_timestamp: bool) {
    let before = advance_timestamp.then(|| date_captions(driver));
    if advance_timestamp {
        // Only wall time advances: the ordinary action stamps a new minute
        // while the production monotonic animation clock continues running.
        pin_edit_clock(1_790_000_120);
    }
    driver.key(key);
    if let Some(before) = before {
        assert!(
            !before.is_empty(),
            "the actual note-date caption is observed"
        );
        assert_ne!(
            before,
            date_captions(driver),
            "the ordinary edit updates the painted metadata caption"
        );
    }
}
