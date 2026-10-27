//! Performing an adapter's action, one step at a time.
//!
//! An [`AdapterInvocation`](crate::AdapterInvocation) is the value an
//! adapter's [`ActionSpec`](crate::ActionSpec) builds: the app, the action
//! and the arguments already bound. This module turns it into what would
//! actually be done — a program run, a bus call made — in the vocabulary of
//! the layer the adapter planned ([`AdapterAction::plan`](crate::AdapterAction::plan)).
//!
//! # The line that matters on Linux
//!
//! An adapter's steps are *the app's own* interfaces: a D-Bus method on the
//! session bus, a command line. A Tilo app runs in a sandbox whose session
//! bus is a filtered proxy: it reaches the portals, notifications and media
//! players its manifest declares, and nothing else. So the question "does
//! this step work from here" is answered by the launcher's filter, not by
//! a judgement made in this crate, and a step it cannot make is *refused
//! here* rather than retried later somewhere else. Two consequences follow,
//! and both are properties of the lowering:
//!
//! * [`Run`](Step::Run) is a program with an argument vector, never a
//!   string a shell parses: nothing in a parameter can become a command.
//! * [`Call`](Step::Call) is a bus name, an object path, an interface and a
//!   method, each of which must be spelled as that kind of name, plus typed
//!   arguments. There is no spelling of a parameter that reaches a different
//!   object.

use serde_json::{Map, Value};

use crate::adapter::{AdapterAction, Check, Layer, Step, Undo};
use crate::params::{ActionParam, ParamType};
use crate::{ADAPTER_OPERATION_TYPE, AdapterInvocation, EncodedAction};

/// One thing to do, after an adapter's declarations have been read and the
/// arguments put in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Work {
    /// Run `program` with `args`, exactly as written: no shell, no
    /// word splitting, no expansion of what a parameter held.
    Run {
        /// The program, from the adapter's declaration.
        program: String,
        /// Its arguments, in order.
        args: Vec<String>,
        /// What to give its standard input, when the adapter declared it.
        stdin: Option<String>,
    },
    /// Call one method on a bus, with its arguments as typed values.
    Call(BusCall),
}

/// One D-Bus method call, every part of which is a name of the kind it
/// claims to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusCall {
    /// The bus name (`org.example.App`).
    pub bus: String,
    /// The object path (`/org/example/App`).
    pub path: String,
    /// The interface (`org.example.App.Documents`).
    pub interface: String,
    /// The method (`Export`).
    pub method: String,
    /// Its arguments, in order.
    pub arguments: Vec<Value>,
}

impl BusCall {
    /// The whole call as one line a person can read and a bus client can
    /// run: `org.example.App /org/example/App org.example.App.Documents
    /// Export -- arg …`, with each argument as its JSON.
    ///
    /// It is spelled as data rather than as something to paste into a shell
    /// on purpose: this is the form a host hands a bus client whose argument
    /// vector it already built.
    #[must_use]
    pub fn to_request(&self) -> Vec<String> {
        let mut request = vec![
            self.bus.clone(),
            self.path.clone(),
            self.interface.clone(),
            self.method.clone(),
            "--".to_owned(),
        ];
        request.extend(self.arguments.iter().map(std::string::ToString::to_string));
        request
    }
}

/// Why an adapter's action could not be planned into work at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The encoded operation is not an adapter invocation.
    NotAnAdapter(String),
    /// The invocation names no adapter on this host.
    UnknownApp(String),
    /// The adapter declares no such action.
    UnknownAction {
        /// The app.
        app: String,
        /// The action id it asked for.
        action: String,
    },
    /// No implementation of the action can be reached here, and each that
    /// was passed over says why.
    NoReachableLayer(Vec<crate::Skipped>),
    /// A required parameter is missing.
    Missing(Vec<String>),
    /// The declaration holds a step this lowering does not perform.
    Unsupported {
        /// Which step, by its layer's word.
        step: &'static str,
        /// Why it is not performed here.
        reason: String,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnAdapter(type_id) => {
                write!(f, "`{type_id}` is not an adapter invocation")
            }
            Self::UnknownApp(app) => write!(f, "no adapter for `{app}` is installed here"),
            Self::UnknownAction { app, action } => {
                write!(f, "the adapter for `{app}` declares no action `{action}`")
            }
            Self::NoReachableLayer(skipped) => {
                if skipped.is_empty() {
                    write!(f, "the action declares no implementation that performs it")
                } else {
                    let reasons: Vec<&str> = skipped.iter().map(|s| s.reason.as_str()).collect();
                    write!(
                        f,
                        "no layer of this action works here: {}",
                        reasons.join("; ")
                    )
                }
            }
            Self::Missing(missing) => write!(f, "{}", missing.join("; ")),
            Self::Unsupported { step, reason } => {
                write!(f, "a `{step}` step is not performed here: {reason}")
            }
        }
    }
}

/// What performing an action needs, once it has been planned: the steps of
/// the chosen implementation, and the two facts about it the caller needs
/// afterwards — what Undo means, and what the layer reports so its checks
/// can be evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The layer this plan runs on.
    pub layer: Layer,
    /// Every step, in order.
    pub steps: Vec<Work>,
    /// What must be true once they have run.
    pub checks: Vec<Check>,
    /// What Undo means for this action.
    pub undo: Undo,
}

impl Plan {
    /// The work with the arguments of one invocation put in place, and the
    /// plan read from `adapter`'s declaration of `action`.
    ///
    /// `reachable` is [`AdapterAction::plan`](crate::AdapterAction::plan)'s:
    /// whether a layer can be reached in the caller's circumstances at all.
    ///
    /// # Errors
    ///
    /// When the application declares no such action, no layer of it is
    /// reachable here, a required argument is missing, or a step is one this
    /// lowering does not perform.
    pub fn lowering(
        adapter: &crate::ActionAdapter,
        invocation: &AdapterInvocation,
        reachable: impl Fn(Layer) -> bool,
    ) -> Result<Self, Refusal> {
        if adapter.app != invocation.app {
            return Err(Refusal::UnknownApp(invocation.app.clone()));
        }
        let action = adapter
            .action(&invocation.action)
            .ok_or_else(|| Refusal::UnknownAction {
                app: invocation.app.clone(),
                action: invocation.action.clone(),
            })?;
        Self::of(action, invocation, reachable)
    }

    /// The plan of `action` for `invocation`'s arguments.
    ///
    /// # Errors
    ///
    /// As [`lowering`](Self::lowering).
    pub fn of(
        action: &AdapterAction,
        invocation: &AdapterInvocation,
        reachable: impl Fn(Layer) -> bool,
    ) -> Result<Self, Refusal> {
        let missing = invocation.missing_arguments(&action.params);
        if !missing.is_empty() {
            return Err(Refusal::Missing(missing));
        }
        let (implementation, skipped) = action.plan(reachable);
        let implementation = implementation.ok_or(Refusal::NoReachableLayer(skipped))?;
        let args = Args {
            declared: &action.params,
            bound: &invocation.arguments,
        };
        let steps = implementation
            .steps
            .iter()
            .map(|step| lower(step, &args))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            layer: implementation.layer,
            steps,
            checks: implementation.checks.clone(),
            undo: action.undo,
        })
    }

    /// Whether every step is a `Run`, so a caller that can only run programs
    /// can perform the whole plan.
    #[must_use]
    pub fn runs_only(&self) -> bool {
        self.steps
            .iter()
            .all(|step| matches!(step, Work::Run { .. }))
    }

    /// Every `Run` in the plan, in order, as the program and its fully
    /// substituted argument vector.
    #[must_use]
    pub fn runs(&self) -> Vec<(&str, &[String])> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Work::Run { program, args, .. } => Some((program.as_str(), args.as_slice())),
                Work::Call(_) => None,
            })
            .collect()
    }

    /// The bus calls in the plan, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<&BusCall> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Work::Call(call) => Some(call),
                Work::Run { .. } => None,
            })
            .collect()
    }
}

/// An invocation read back from the encoded operation an executor was
/// handed, with the adapter that declares it.
struct Args<'a> {
    declared: &'a [ActionParam],
    bound: &'a Map<String, Value>,
}

impl Args<'_> {
    /// The declared types that fill the `{{param}}` and `{{steps.N.content}}`
    /// placeholders of `text`.
    fn fill(&self, text: &str) -> Result<String, Refusal> {
        let mut filled = String::new();
        let mut rest = text;
        while let Some((before, after)) = rest.split_once("{{") {
            filled.push_str(before);
            let (placeholder, tail) =
                after.split_once("}}").ok_or_else(|| Refusal::Unsupported {
                    step: "run",
                    reason: "a `{{` placeholder is never closed".into(),
                })?;
            filled.push_str(&self.value_of(placeholder.trim())?);
            rest = tail;
        }
        filled.push_str(rest);
        Ok(filled)
    }

    /// The text a placeholder stands for: a parameter's value, or the
    /// content of an earlier step's result.
    fn value_of(&self, placeholder: &str) -> Result<String, Refusal> {
        let value = match placeholder.split_once('.') {
            Some(("param" | "input", name)) => self
                .declared
                .iter()
                .find(|param| param.name.as_ref() == name)
                .and_then(|param| self.bound.get(param.name.as_ref()))
                .cloned(),
            // A step's own result is written by the program the step ran;
            // an adapter's declaration never puts one back into a later
            // argument, because the two programs would then talk through a
            // pipe this lowering does not draw.
            Some(("steps", _)) => None,
            _ => {
                return Err(Refusal::Unsupported {
                    step: "run",
                    reason: format!("`{{{{{placeholder}}}}}` is not a parameter"),
                });
            }
        };
        let Some(value) = value else {
            return Ok(String::new());
        };
        Ok(match value {
            Value::String(text) => text,
            other => other.to_string(),
        })
    }
}

/// The work one declared step is, with the arguments in place.
fn lower(step: &Step, args: &Args<'_>) -> Result<Work, Refusal> {
    match step {
        Step::Run {
            program,
            args: declared,
            stdin,
        } => {
            let filled = declared
                .iter()
                .map(|argument| args.fill(argument))
                .collect::<Result<Vec<_>, _>>()?;
            let input = match stdin.as_deref() {
                Some(text) => Some(args.fill(text)?),
                None => None,
            };
            Ok(Work::Run {
                program: program.clone(),
                args: filled,
                stdin: input,
            })
        }
        Step::Call {
            bus,
            path,
            interface,
            method,
            arguments,
        } => {
            check_bus_name(bus)?;
            check_object_path(path)?;
            check_interface(interface)?;
            check_member(method)?;
            Ok(Work::Call(BusCall {
                bus: bus.clone(),
                path: path.clone(),
                interface: interface.clone(),
                method: method.clone(),
                arguments: arguments.clone(),
            }))
        }
        Step::Request { .. } => Err(Refusal::Unsupported {
            step: "request",
            reason: "an HTTP endpoint is not one of the interfaces a Linux app is wrapped by; \
                     an adapter that needs one declares the layer that reaches it"
                .into(),
        }),
        Step::Tool { .. } => Err(Refusal::Unsupported {
            step: "tool",
            reason: "an MCP tool is reached through the connector that names its server".into(),
        }),
        Step::Edit { .. } => Err(Refusal::Unsupported {
            step: "edit",
            reason: "a resource edit is applied by the resource layer, not by an adapter's \
                     program"
                .into(),
        }),
        Step::Invoke { .. } | Step::Press { .. } | Step::Click { .. } | Step::Type { .. } => {
            Err(Refusal::Unsupported {
                step: step.required_layer().map_or("ui", Layer::name),
                reason: "driving an app's UI is not performed through an adapter's interface \
                         layer"
                    .into(),
            })
        }
    }
}

/// Whether `name` is a D-Bus bus name: dot-separated elements, each
/// starting with a letter or `_`, holding only letters, digits, `_` and `-`,
/// with at least two elements.
#[must_use]
pub fn is_bus_name(name: &str) -> bool {
    let elements: Vec<&str> = name.split('.').collect();
    elements.len() >= 2
        && elements.iter().all(|element| {
            !element.is_empty()
                && element
                    .bytes()
                    .next()
                    .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
                && element
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
}

fn check_bus_name(name: &str) -> Result<(), Refusal> {
    if is_bus_name(name) {
        Ok(())
    } else {
        Err(Refusal::Unsupported {
            step: "call",
            reason: format!("`{name}` is not a bus name"),
        })
    }
}

fn check_object_path(path: &str) -> Result<(), Refusal> {
    let valid = path.starts_with('/')
        && path.split('/').skip(1).all(|element| {
            element
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
    if valid {
        Ok(())
    } else {
        Err(Refusal::Unsupported {
            step: "call",
            reason: format!("`{path}` is not an object path"),
        })
    }
}

fn check_interface(interface: &str) -> Result<(), Refusal> {
    if is_bus_name(interface) {
        Ok(())
    } else {
        Err(Refusal::Unsupported {
            step: "call",
            reason: format!("`{interface}` is not an interface name"),
        })
    }
}

fn check_member(member: &str) -> Result<(), Refusal> {
    let valid = !member.is_empty()
        && member
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && member
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(Refusal::Unsupported {
            step: "call",
            reason: format!("`{member}` is not a method name"),
        })
    }
}

/// Whether a parameter's declared type can be carried unaltered into a bus
/// call argument. Text, numbers, booleans, objects, arrays and dates map
/// onto D-Bus types; a resource or a secret does not become a bus value on
/// its own.
#[must_use]
pub const fn is_bus_value(ty: ParamType) -> bool {
    matches!(
        ty,
        ParamType::Text
            | ParamType::Integer
            | ParamType::Number
            | ParamType::Boolean
            | ParamType::Object
            | ParamType::Array
            | ParamType::DateTime
    )
}

/// The type id of an encoded adapter invocation, so a host that hands
/// adapters' work to its own performers can recognise one.
#[must_use]
pub const fn operation_type() -> &'static str {
    ADAPTER_OPERATION_TYPE
}

/// The invocation `operation` carries.
///
/// # Errors
///
/// When it is not an adapter invocation, or its payload does not decode.
pub fn invocation_of(operation: &EncodedAction) -> Result<AdapterInvocation, Refusal> {
    AdapterInvocation::from_encoded(operation)
        .map_err(|_| Refusal::NotAnAdapter(operation.type_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_names_are_read_as_their_grammar_allows() {
        assert!(is_bus_name("org.example.App"));
        assert!(is_bus_name("org.freedesktop.portal.Screenshot"));
        assert!(!is_bus_name("org"));
        assert!(!is_bus_name("org.1example"));
        assert!(!is_bus_name("org..example"));
        assert!(!is_bus_name("org.example.App;rm -rf /"));
    }
}
