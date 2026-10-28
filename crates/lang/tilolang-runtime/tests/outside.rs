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
