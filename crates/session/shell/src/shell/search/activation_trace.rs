//! Transient input-to-invocation correlation for the ordinary palette door.
//! This metadata grants no authority and never enters action history.
use serde_json::Value;
use std::cell::Cell;
use tilo_actions::InvocationId;

#[derive(Clone, Copy)]
struct Origin {
    source: &'static str,
    dispatch_ns: u128,
    input_ns: Option<u64>,
    palette: Option<super::palette_trace::InputIdentity>,
}
thread_local! { static TARGET: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) }; }
thread_local! { static ORIGIN: Cell<Option<Origin>> = const { Cell::new(None) }; }

pub(crate) struct Scope {
    previous: Option<Origin>,
    changed: bool,
}
impl Drop for Scope {
    fn drop(&mut self) {
        if self.changed {
            ORIGIN.set(self.previous);
        }
    }
}
impl Scope {
    pub(crate) fn palette(identity: super::palette_trace::InputIdentity) -> Self {
        let previous = ORIGIN.get();
        ORIGIN.set(Some(Origin {
            source: identity.kind,
            dispatch_ns: identity.dispatch_ns,
            input_ns: Some(identity.input_ns),
            palette: Some(identity),
        }));
        Self {
            previous,
            changed: true,
        }
    }
    pub(crate) fn pointer_button(input_ns: Option<u64>) -> Self {
        Self::enter(
            if input_ns.is_some() {
                "physical_pointer_button"
            } else {
                "native_unstamped"
            },
            input_ns,
            false,
        )
    }
    pub(crate) fn native(input_ns: Option<u64>) -> Self {
        Self::enter(
            if input_ns.is_some() {
                "physical_key"
            } else {
                "native_unstamped"
            },
            input_ns,
            false,
        )
    }
    pub(crate) fn controlled() -> Self {
        Self::enter("controlled_input", None, true)
    }
    fn enter(source: &'static str, input_ns: Option<u64>, preserve: bool) -> Self {
        let previous = ORIGIN.get();
        let origin = crate::shell::launcher::LaunchSession::current()
            .presentation_observer_enabled()
            .then(crate::compositor::client_input::monotonic_time_ns)
            .flatten()
            .map(|dispatch_ns| Origin {
                source,
                dispatch_ns,
                input_ns,
                palette: None,
            });
        let changed = origin.is_some() && !(preserve && previous.is_some());
        if changed {
            ORIGIN.set(origin);
        }
        Self { previous, changed }
    }
}

/// Exact selected search identity around the ordinary callback, not an inferred pane.
pub(crate) struct TargetScope(Option<(String, String)>);
impl TargetScope {
    pub(crate) fn enter(provider: &str, action: &str) -> Self {
        let previous = TARGET.with(|target| target.borrow_mut().take());
        if ORIGIN.get().is_some_and(|origin| origin.palette.is_some()) {
            TARGET.with(|target| *target.borrow_mut() = Some((provider.into(), action.into())));
        }
        Self(previous)
    }
}
impl Drop for TargetScope {
    fn drop(&mut self) {
        TARGET.with(|target| *target.borrow_mut() = self.0.take());
    }
}

fn row(invocation: InvocationId, origin: Option<Origin>, now_ns: u128) -> Value {
    serde_json::json!({
        "invocation": invocation, "application": "so.tilo.settings",
        "source": origin.map_or("semantic", |value| value.source),
        "clock_id": libc::CLOCK_MONOTONIC,
        "activation_dispatch_ns": origin.map(|value| value.dispatch_ns),
        "input_time_ns": origin.and_then(|value| value.input_ns).filter(|time| u128::from(*time) <= now_ns),
        "palette_input_id": origin.and_then(|value| value.palette).map(|value| value.id),
        "palette_observer_session": origin.and_then(|value| value.palette).map(|value| value.session),
        "palette_target_provider": TARGET.with(|target| target.borrow().as_ref().map(|value| value.0.clone())),
        "palette_target_action": TARGET.with(|target| target.borrow().as_ref().map(|value| value.1.clone())),
        "submission_time_ns": now_ns,
        "process_id": std::process::id(),
    })
}

pub(crate) fn submitted(application: &str, invocation: InvocationId) {
    if application != "so.tilo.settings"
        || !crate::shell::launcher::LaunchSession::current().presentation_observer_enabled()
    {
        return;
    }
    if let Some(now_ns) = crate::compositor::client_input::monotonic_time_ns() {
        eprintln!(
            "tilo-action-submission {}",
            row(invocation, ORIGIN.get(), now_ns)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_submission_distinguishes_actual_input_and_semantic_dispatch() {
        let invocation = InvocationId::new();
        let physical = row(
            invocation,
            Some(Origin {
                source: "physical_key",
                dispatch_ns: 200,
                input_ns: Some(100),
                palette: None,
            }),
            300,
        );
        assert_eq!(physical["invocation"], serde_json::json!(invocation));
        assert_eq!(physical["input_time_ns"], 100);
        assert_eq!(physical["activation_dispatch_ns"], 200);
        assert_eq!(physical["submission_time_ns"], 300);
        for source in ["native_unstamped", "controlled_input"] {
            let observed = row(
                invocation,
                Some(Origin {
                    source,
                    dispatch_ns: 200,
                    input_ns: None,
                    palette: None,
                }),
                300,
            );
            assert_eq!(observed["source"], source);
            assert!(observed["input_time_ns"].is_null());
        }
        let semantic = row(invocation, None, 300);
        assert_eq!(semantic["source"], "semantic");
        assert!(semantic["activation_dispatch_ns"].is_null());
        assert!(semantic["input_time_ns"].is_null());
        assert!(physical.get("query").is_none());
        assert!(physical.get("key").is_none());
        let impossible = row(
            invocation,
            Some(Origin {
                source: "physical_key",
                dispatch_ns: 200,
                input_ns: Some(400),
                palette: None,
            }),
            300,
        );
        assert!(impossible["input_time_ns"].is_null());
    }

    #[test]
    fn palette_click_submission_preserves_kernel_origin_and_exact_selected_action() {
        assert!(ORIGIN.get().is_none());
        let identity = super::super::palette_trace::InputIdentity {
            session: uuid::Uuid::new_v4(),
            id: 7,
            kind: "physical_pointer_button",
            input_ns: 10,
            dispatch_ns: 20,
        };
        {
            let _origin = Scope::palette(identity);
            let _target = TargetScope::enter("app-actions", "settings.section:bluetooth/open-pane");
            let observed = row(InvocationId::new(), ORIGIN.get(), 30);
            assert_eq!(observed["source"], "physical_pointer_button");
            assert_eq!(observed["input_time_ns"], 10);
            assert_eq!(observed["activation_dispatch_ns"], 20);
            assert_eq!(observed["palette_input_id"], 7);
            assert_eq!(
                observed["palette_observer_session"],
                serde_json::json!(identity.session)
            );
            assert_eq!(observed["palette_target_provider"], "app-actions");
            assert_eq!(
                observed["palette_target_action"],
                "settings.section:bluetooth/open-pane"
            );
        }
        let semantic = row(InvocationId::new(), ORIGIN.get(), 40);
        assert!(semantic["palette_input_id"].is_null());
        assert!(semantic["palette_target_action"].is_null());
        assert!(ORIGIN.get().is_none());
    }

    #[test]
    fn action_submission_origin_is_scoped_without_replacing_outer_native_origin() {
        assert!(ORIGIN.get().is_none());
        let native = Origin {
            source: "physical_key",
            dispatch_ns: 2,
            input_ns: Some(1),
            palette: None,
        };
        ORIGIN.set(Some(native));
        {
            let _inner = Scope {
                previous: Some(native),
                changed: false,
            };
            assert_eq!(ORIGIN.get().unwrap().source, "physical_key");
        }
        {
            let _outer = Scope {
                previous: None,
                changed: true,
            };
            assert_eq!(
                row(InvocationId::new(), ORIGIN.get(), 3)["source"],
                "physical_key"
            );
        }
        assert!(ORIGIN.get().is_none());
    }
}
