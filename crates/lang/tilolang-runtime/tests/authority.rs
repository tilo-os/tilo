//! Authority follows authorship: a stranger's button asks, the app's own view is allowed,
//! and revoking trust changes the next decision.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{InvokeOutcome, Value};

    struct Setup {
        w: World,
        stranger: tilolang_runtime::PackageId,
        c: tilolang_runtime::ResourceRef,
    }

    fn setup() -> Setup {
        let w = World::new();
        let stranger =
            w.rt.load_package(&stranger_wasm(), stranger_meta())
                .unwrap();
        let c = w.counter();
        Setup { w, stranger, c }
    }

    tilo_test_env::scenario! {
        fn the_authority_decides_who_may_run_an_action;

        phase a_stranger_button_asks_and_the_apps_own_view_is_allowed;
        {
            let Setup { w, stranger, c } = setup();

            let own =
                w.rt.open_view(&w.me, "vc", vec![Value::resource(&c)])
                    .unwrap();
            assert!(matches!(own.press("btn").unwrap(), InvokeOutcome::Done(_)));
            assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

            let theirs =
                w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
                    .unwrap();
            let InvokeOutcome::Asked(_) = theirs.press("btn").unwrap() else {
                panic!("a foreign invocation from a stranger's view must ask");
            };
            let asked = w.dispatch.waiting();
            let tilolang_runtime::delegate::Invocation::Action {
                action, author, asks, ..
            } = asked
            else {
                panic!("the ask is an action");
            };
            assert_eq!(action, "inc");
            assert!(asks, "the dispatch is told a person must be asked");
            assert_eq!(
                author,
                Some(stranger),
                "the dispatch names the code that asked"
            );
            assert_eq!(
                w.rt.read(&c, "value").unwrap(),
                Value::Int(1),
                "asking changes nothing"
            );
        }

        phase revoking_trust_changes_the_decision_at_the_next_invocation;
        {
            let Setup { w, stranger, c } = setup();
            let theirs =
                w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
                    .unwrap();

            assert!(matches!(
                theirs.press("btn").unwrap(),
                InvokeOutcome::Asked(_)
            ));

            w.auth.trust(&stranger);
            assert!(
                matches!(theirs.press("btn").unwrap(), InvokeOutcome::Done(_)),
                "trusted authors are allowed"
            );
            assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

            w.auth.revoke(&stranger);
            assert!(
                matches!(theirs.press("btn").unwrap(), InvokeOutcome::Asked(_)),
                "the same open view, the same button: revoked at once"
            );
            assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));

            w.auth.trust(&stranger);
            w.auth.deny_module(&stranger);
            assert_eq!(
                theirs.press("btn").unwrap(),
                InvokeOutcome::Denied,
                "deny wins"
            );
        }

        phase a_host_with_no_dispatch_refuses_the_call;
        {
            let w = World::bare();
            w.rt.load_package(&stranger_wasm(), stranger_meta()).unwrap();
            let c = w.counter();
            let theirs =
                w.rt.open_view(&w.me, "vs", vec![Value::resource(&c)])
                    .unwrap();
            assert_eq!(
                theirs.press("btn").unwrap(),
                InvokeOutcome::Denied,
                "nobody here can be asked"
            );
            assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(0));
        }

        phase a_direct_call_by_a_person_is_allowed;
        {
            let Setup { w, c, .. } = setup();
            let out =
                w.rt.invoke_action(&w.me, ("inc", vec![Value::resource(&c)]))
                    .unwrap();
            assert_eq!(out, InvokeOutcome::Done(Value::Unit));
            w.auth.deny_action("inc");
            let out =
                w.rt.invoke_action(&w.me, ("inc", vec![Value::resource(&c)]))
                    .unwrap();
            assert_eq!(out, InvokeOutcome::Denied);
        }
    }
}
