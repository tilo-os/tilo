//! Bound components' controls follow their policy clock: walked again when
//! a grant ends by it, in a window of its own and in a host's scene, and
//! when a clock that could not be read can be again.

use super::*;

/// A bound component whose grant ends at a time of its policy clock has its
/// controls walked again then, and not before.
#[test]
fn bound_controls_wake_when_a_grant_ends_by_the_policy_clock() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let clock = Arc::new(AtomicU64::new(100));
    let mut fixture = ActionFixture::new();
    let sampled = clock.clone();
    fixture.permissions.set(
        crate::permissions::PermissionContext::granted(
            "fixture-user",
            &crate::permissions::TreePath::root(),
            crate::permissions::policy::KindSet::ALL,
            Some(110),
        )
        .with_policy_time_sampler(Arc::new(move || Ok(sampled.load(Ordering::SeqCst)))),
    );
    CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
    let start = crate::time::now();
    crate::time::testing::with_monotonic_time(start, || {
        fixture.app.refresh_control_action_availability();
    });
    assert_eq!(
        CONTROL_AUTHORITY.with_borrow(Clone::clone),
        vec![(false, true), (true, true)]
    );
    let ends = start + std::time::Duration::from_secs(10);
    assert_eq!(fixture.app.permissions_change_at(), Some(ends));
    CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
    crate::time::testing::with_monotonic_time(ends - std::time::Duration::from_millis(1), || {
        fixture.app.refresh_control_action_availability();
    });
    assert!(CONTROL_AUTHORITY.with_borrow(Vec::is_empty));
    clock.store(110, Ordering::SeqCst);
    crate::time::testing::with_monotonic_time(ends, || {
        fixture.app.refresh_control_action_availability();
    });
    assert_eq!(
        CONTROL_AUTHORITY.with_borrow(Clone::clone),
        vec![(false, false), (true, false)]
    );
    assert_eq!(fixture.app.permissions_change_at(), None);
}

/// An app shown in a host's scene follows its bound components' policy
/// clock as a window of its own does: a clock that cannot be read keeps
/// their controls refused until its host announces it can be read again,
/// which wakes the app, and a grant ending by the clock is a deadline the
/// app reports.
#[test]
fn an_embedded_app_follows_its_policy_clock_through_recovery_and_deadline() {
    use crate::time::testing::with_monotonic_time;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;
    let clock = Arc::new(AtomicU64::new(100));
    let readable = Arc::new(AtomicBool::new(false));
    let fixture = ActionFixture::scoped(crate::wake::WakeScope::new());
    let (sampled, read) = (clock.clone(), readable.clone());
    fixture.permissions.set(
        crate::permissions::PermissionContext::granted(
            "fixture-user",
            &crate::permissions::TreePath::root(),
            crate::permissions::policy::KindSet::ALL,
            Some(110),
        )
        .with_policy_time_sampler(Arc::new(move || {
            if read.load(Ordering::SeqCst) {
                Ok(sampled.load(Ordering::SeqCst))
            } else {
                Err("the policy clock cannot be read".into())
            }
        })),
    );
    let ActionFixture {
        _temp,
        app,
        records: _records,
        bindings: _bindings,
        permissions: _permissions,
    } = fixture;
    let mut tile = crate::app::EmbeddedApp::new(app, |_| {}).unwrap();
    let theme = crate::theme::Theme::light();
    let model = automerge::AutoCommit::new();
    let mut taffy = taffy::TaffyTree::new();
    let node = tile.build_layout(&mut taffy, &theme, &mut std::collections::HashMap::new());
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(300.0),
                height: taffy::AvailableSpace::Definite(100.0),
            },
        )
        .unwrap();
    tile.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
    let paint = |tile: &mut crate::app::EmbeddedApp| {
        tile.draw(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut crate::text_render::TextRenderer::new(),
            &theme,
            &crate::presence::PresenceCtx::empty(),
        );
    };
    let start = crate::time::now();

    CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
    with_monotonic_time(start, || paint(&mut tile));
    assert_eq!(
        CONTROL_AUTHORITY.with_borrow(Clone::clone),
        vec![(false, false), (true, false)],
        "no clock, no grant"
    );
    assert_eq!(tile.next_wake_deadline(), None, "nothing to wake for");

    readable.store(true, Ordering::SeqCst);
    crate::permissions::outside_changed();
    CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
    with_monotonic_time(start, || {
        assert!(tile.update(&model, &ROOT), "the announcement wakes the app");
        paint(&mut tile);
    });
    assert_eq!(
        CONTROL_AUTHORITY.with_borrow(Clone::clone),
        vec![(false, true), (true, true)],
        "the clock read again grants"
    );

    let ends = start + Duration::from_secs(10);
    assert_eq!(tile.next_wake_deadline(), Some(ends));
    clock.store(110, Ordering::SeqCst);
    CONTROL_AUTHORITY.with_borrow_mut(Vec::clear);
    with_monotonic_time(ends, || {
        assert!(tile.update(&model, &ROOT), "the deadline runs the app");
        paint(&mut tile);
    });
    assert_eq!(
        CONTROL_AUTHORITY.with_borrow(Clone::clone),
        vec![(false, false), (true, false)],
        "the grant ended"
    );
    assert_eq!(tile.next_wake_deadline(), None);
}
