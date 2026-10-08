//! Damage from the outer, ordinarily mounted component after real note input.
use crate::component_support::*;
use notes::{actions, doc::NotesDoc};
use std::{cell::RefCell, rc::Rc};
use ui::{Widget, headless::Driver};
use vello::kurbo::Rect;

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
    paragraph_input(true, "y", "xy");
}

#[test]
fn canonical_notes_default_motion_and_word_boundary_preserve_sparse_gpu_pixels() {
    paragraph_input(false, "y", "xy");
    paragraph_input(false, "Space", "x ");
}

fn paragraph_input(reduce_motion: bool, key: &str, prefix: &str) {
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
    assert!(
        driver.settle_for_capture(ui::time::Duration::from_secs(2)),
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
    driver.key(key);
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
    let mut retained = ui::offscreen::OffscreenRenderer::new(1200, 900);
    let mut full = ui::offscreen::OffscreenRenderer::new(1200, 900);
    let old_pixels = retained.render(&before, background);
    let damage = damage
        .iter()
        .map(|rect| rect.inflate(16.0, 16.0))
        .collect::<Vec<_>>();
    let work = retained.render_damage_regions_gpu_wait(driver.scene(), background, &damage);
    assert!(
        work < 1200 * 900,
        "the outer component must exercise actual sparse rasterization"
    );
    let expected = full.render(driver.scene(), background);
    assert_ne!(
        old_pixels, expected,
        "ordinary input must change visible note paint"
    );
    assert_eq!(
        retained.readback_current(),
        expected,
        "all retained app pixels match the full GPU oracle"
    );
    close_driver(&mut driver);
}
