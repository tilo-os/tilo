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
}
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
            });
        let changed = origin.is_some() && !(preserve && previous.is_some());
        if changed {
            ORIGIN.set(origin);
        }
        Self { previous, changed }
    }
}

fn row(invocation: InvocationId, origin: Option<Origin>, now_ns: u128) -> Value {
    serde_json::json!({
        "invocation": invocation, "application": "so.tilo.settings",
        "source": origin.map_or("semantic", |value| value.source),
        "clock_id": libc::CLOCK_MONOTONIC,
        "activation_dispatch_ns": origin.map(|value| value.dispatch_ns),
        "input_time_ns": origin.and_then(|value| value.input_ns).filter(|time| u128::from(*time) <= now_ns),
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
            }),
            300,
        );
        assert!(impossible["input_time_ns"].is_null());
    }

    #[test]
    fn action_submission_origin_is_scoped_without_replacing_outer_native_origin() {
        assert!(ORIGIN.get().is_none());
        let native = Origin {
            source: "physical_key",
            dispatch_ns: 2,
            input_ns: Some(1),
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
