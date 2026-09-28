#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "controlled production headless drain regression"
)]
use super::*;
use crate::bound_dispatch::legacy_document_tests::{SetValue, independent_batch_fixture};
#[test]
fn headless_drain_keeps_independent_document_and_disposable_view_controls() {
    for case in ["closed", "unavailable", "busy"] {
        for failed_first in [false, true] {
            for view in [false, true] {
                let (a, b, root) = independent_batch_fixture(case, failed_first, view);
                let mut driver = Driver::new(State::new(), Box::new(root), (700, 400));
                driver
                    .action_registry_mut()
                    .register::<SetValue>("controlled.legacy");
                let outer = driver.state.doc.save();
                let before_b = b.snapshot();
                a.probe.offer();
                b.probe.offer();
                if case == "busy" {
                    b.store
                        .with_action_state(&b.checkout.id, |_, _, _| driver.host_tick())
                        .unwrap();
                } else {
                    driver.host_tick();
                }
                assert_eq!(
                    a.probe.acks.get(),
                    usize::from(!view),
                    "healthy A must dispatch: {case}, failed_first={failed_first}, view={view}"
                );
                assert_eq!(a.probe.starts.get(), usize::from(view));
                assert_eq!(b.probe.acks.get(), 0);
                assert_eq!(b.probe.starts.get(), 0);
                assert_eq!(b.probe.queue.borrow().len(), 1);
                assert_eq!(b.snapshot(), before_b);
                assert_eq!(driver.state.doc.save(), outer);
                driver.host_tick();
                assert_eq!(a.probe.acks.get(), usize::from(!view));
                assert_eq!(a.probe.starts.get(), usize::from(view));
                assert_eq!(b.probe.acks.get(), usize::from(case == "busy" && !view));
                assert_eq!(b.probe.starts.get(), usize::from(case == "busy" && view));
            }
        }
    }
}
