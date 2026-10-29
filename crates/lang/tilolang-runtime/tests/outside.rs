//! Outside effects: queued in the action's change, delivered when settled.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use serde_json::json;
    use tilolang_runtime::{InvokeOutcome, MemorySink, Value};

    fn world() -> (World, tilolang_runtime::ResourceRef) {
        let w = World::new();
        w.rt.load_package(&mail_wasm(), mail_meta()).unwrap();
        let m = w.rt.create("Mail").unwrap();
        w.rt.invoke_action(
            &w.me,
            (
                "set_to",
                vec![Value::resource(&m), Value::text("alice@example.org")],
            ),
        )
        .unwrap();
        (w, m)
    }

    #[test]
    fn an_outside_action_asks_until_granted_then_queues_and_settles() {
        let (w, m) = world();
        let send = || {
            w.rt.invoke_action(&w.me, ("send", vec![Value::resource(&m)]))
                .unwrap()
        };

        assert!(
            matches!(send(), InvokeOutcome::Suggested(_)),
            "outside effects ask by default"
        );
        assert!(w.rt.pending_outbox().is_empty());

        w.auth.grant_outside("send");
        assert!(matches!(send(), InvokeOutcome::Done(_)));

        let pending = w.rt.pending_outbox();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].capability, "post");
        assert_eq!(pending[0].payload, json!("alice@example.org"));
        let change = w.rt.history(&m.doc).unwrap();
        assert!(
            change.iter().any(|n| n.action == "send"),
            "the outbox entry travels in the action's own change: {change:?}"
        );

        let sink = MemorySink::new();
        w.rt.set_sink(sink.clone());
        // First it is marked as being handed over, a change of its own; then handed over.
        let report = w.rt.settle().unwrap();
        assert_eq!((report.marked, report.delivered), (1, 0));
        assert!(
            sink.delivered().is_empty(),
            "nothing leaves before the mark"
        );
        assert_eq!(w.rt.pending_outbox().len(), 1);
        let report = w.rt.settle().unwrap();
        assert_eq!(report.delivered, 1);
        assert!(report.failed.is_empty());
        assert_eq!(
            sink.delivered(),
            vec![("post".to_owned(), json!("alice@example.org"))]
        );
        assert!(w.rt.pending_outbox().is_empty());
        assert_eq!(w.rt.settle().unwrap().delivered, 0, "settled once");
    }

    /// A sink that answers what the test scripted, one answer per hand-over.
    #[derive(Clone, Default)]
    struct Scripted(std::rc::Rc<std::cell::RefCell<Vec<tilolang_runtime::Delivery>>>);

    impl tilolang_runtime::OutsideSink for Scripted {
        fn deliver(&mut self, _: &str, _: &serde_json::Value) -> tilolang_runtime::Delivery {
            self.0.borrow_mut().remove(0)
        }
    }

    #[test]
    fn an_unreachable_service_waits_to_go_online_and_a_refusal_is_final() {
        use tilolang_runtime::Delivery;
        let (w, m) = world();
        w.auth.grant_outside("send");
        w.rt.invoke_action(&w.me, ("send", vec![Value::resource(&m)]))
            .unwrap();
        let sink = Scripted::default();
        w.rt.set_sink(sink.clone());
        sink.0.borrow_mut().extend([
            Delivery::Offline("no route to the server".into()),
            Delivery::Pending,
            Delivery::Rejected("mailbox unavailable".into()),
        ]);
        w.rt.settle().unwrap();
        // The server cannot be reached: it waits, and is not tried again on its own.
        assert_eq!(w.rt.settle().unwrap().waiting, 1);
        for _ in 0..3 {
            assert_eq!(
                w.rt.settle().unwrap(),
                tilolang_runtime::SettleReport::default(),
                "it waits"
            );
        }
        assert_eq!(sink.0.borrow().len(), 2, "handed over once");
        // The network came back: marked again, handed over, no answer yet, then refused.
        w.rt.retry_waiting();
        assert_eq!(w.rt.settle().unwrap().marked, 1);
        assert_eq!(
            w.rt.settle().unwrap(),
            tilolang_runtime::SettleReport::default(),
            "no answer yet"
        );
        let report = w.rt.settle().unwrap();
        assert_eq!(
            report.failed,
            vec![(report.failed[0].0.clone(), "mailbox unavailable".to_owned())]
        );
        assert!(w.rt.pending_outbox().is_empty(), "a refusal is final");
        assert_eq!(
            w.rt.settle().unwrap(),
            tilolang_runtime::SettleReport::default()
        );
    }

    #[test]
    fn a_rolled_back_action_queues_nothing() {
        let (w, m) = world();
        w.auth.grant_outside("send_bad");
        let err =
            w.rt.invoke_action(&w.me, ("send_bad", vec![Value::resource(&m)]))
                .unwrap_err();
        assert!(err.to_string().contains("rolled back"), "{err}");
        assert!(w.rt.pending_outbox().is_empty());
    }

    #[test]
    fn only_an_action_classed_outside_may_call_outside() {
        let (w, m) = world();
        let err =
            w.rt.invoke_action(&w.me, ("sneak", vec![Value::resource(&m)]))
                .unwrap_err();
        assert!(err.to_string().contains("not classed outside"), "{err}");
        assert!(w.rt.pending_outbox().is_empty());
    }

    #[test]
    fn settling_without_a_sink_is_an_error_and_keeps_the_entry() {
        let (w, m) = world();
        w.auth.grant_outside("send");
        w.rt.invoke_action(&w.me, ("send", vec![Value::resource(&m)]))
            .unwrap();
        assert!(w.rt.settle().is_err());
        assert_eq!(w.rt.pending_outbox().len(), 1);
    }
}
