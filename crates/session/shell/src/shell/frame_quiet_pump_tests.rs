//! The model pump's launchpad settle and what a wake that changes nothing
//! costs it.

use super::*;
use automerge::ReadDoc as _;

/// A wake that changes nothing still runs the model pump. On the
/// populated desktop that pump must announce no tree write (every
/// generation-gated phase of the loop would walk the tree again on the
/// next wake) and must not probe the widget document for launchpad
/// requests it already found absent. A request and a real tree write
/// still land on the pump after them.
#[test]
fn a_quiet_pump_announces_no_tree_write_and_reads_no_launchpad_request() {
    let harvests = |phases: &[tilo_resource::profiling::Sample]| {
        phases
            .iter()
            .find(|sample| sample.phase == "shell.launchpad_harvest")
            .map_or(0, |sample| sample.calls)
    };
    // The top bar's minute would be a real change between two pumps.
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = crate::shell::test_support::PopulatedShellFixture::new(
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    frame.relayout();
    let _ = frame.paint_with_damage();
    let mut doc = AutoCommit::new();
    assert!(
        (0..50).any(|_| !frame.pump_apps(&mut doc, &automerge::ROOT)),
        "the populated desktop comes to rest"
    );

    let tree = frame.chrome().tree_handle();
    let generation = tree.generation();
    let revision = frame.chrome_revision();
    let ((), phases) = tilo_resource::profiling::observe(|| {
        for _ in 0..20 {
            assert!(
                !frame.pump_apps(&mut doc, &automerge::ROOT),
                "nothing changed on a quiet desktop"
            );
        }
    });
    assert_eq!(
        tree.generation(),
        generation,
        "a quiet pump announced a tree write"
    );
    assert_eq!(frame.chrome_revision(), revision);
    assert_eq!(
        harvests(&phases),
        0,
        "a quiet pump read the launchpad requests again: {phases:?}"
    );

    // The document moved: the next pump reads it and takes the request.
    doc.put(
        &automerge::ROOT,
        tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP,
        true,
    )
    .expect("queued launcher request");
    let (changed, phases) =
        tilo_resource::profiling::observe(|| frame.pump_apps(&mut doc, &automerge::ROOT));
    assert!(changed, "the taken request repaints");
    assert!(harvests(&phases) > 0, "{phases:?}");
    assert!(frame.host.palette_visible(), "the launcher opened");

    // A write to the tree still announces itself.
    let before = tree.generation();
    frame.with_chrome(|chrome| {
        chrome.add_workspace("quiet pump");
    });
    drop(frame);
    assert_ne!(tree.generation(), before, "a real tree write is announced");
    drop(clock);
}

#[test]
fn frame_pump_settles_a_launchpad_request_once() {
    let mut frame = FrameBuilder::new((1280, 800));
    let mut doc = AutoCommit::new();
    doc.put(
        &automerge::ROOT,
        tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP,
        true,
    )
    .expect("queued launcher request");

    assert!(
        frame.pump_apps(&mut doc, &automerge::ROOT),
        "a settled action requests repaint"
    );
    assert!(
        frame.host.palette_visible(),
        "the production frame pump opens the launcher"
    );
    assert!(
        matches!(
            doc.get(&automerge::ROOT, tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP),
            Ok(Some((automerge::Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(false))
        ),
        "typed settlement consumes the request"
    );
    doc.put(&automerge::ROOT, "pending-observer-witness", "uncommitted")
        .expect("pending fixture edit");
    assert!(doc.pending_ops() > 0);
    let revision = (doc.base_heads(), doc.pending_ops());
    frame.pump_apps(&mut doc, &automerge::ROOT);
    drop(frame);
    assert_eq!(
        revision,
        (doc.base_heads(), doc.pending_ops()),
        "the next pump does not repeat the request"
    );
}
