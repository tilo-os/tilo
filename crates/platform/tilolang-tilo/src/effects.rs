//! What an agent does on the door beyond the published actions of the packages: run a block,
//! run a flow, read the catalogue (implementation design 7.4).
//!
//! They are an [`Effect`] vocabulary published on the `tl:host` resource, so the desktop agent,
//! the runtime door, shortcuts and grants reach them as they reach every other operation. The
//! window answers them with [`Host::answer`]; what they change in the room they change through
//! the runtime's own commits, which the window hands to the door like a press's.

use serde_json::{Value as Json, json};
use tilo_actions::ActionOutput;
use tilo_resource::Effect;
use tilolang_runtime::value::{ResourceRef, to_json};
use tilolang_runtime::{AgentBlock, FlowOutcome, Principal, Resolution, UiNode};

use crate::host::{Host, value_of};
use crate::publish::Caller;

/// The operations the host offers an agent.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "tilolang.host")]
pub enum HostEffect {
    /// Run a block.
    ///
    /// Evaluate a tilolang expression against every app of the room: a view block renders, a
    /// value block is shown as data, and an action block runs or, when it needs a person's
    /// approval, is recorded as a suggested invocation.
    #[effect(returns)]
    RunBlock {
        #[param("The block's source, such as `letters.send(@letters.Draft:id)` or a view body")]
        source: String,
    },
    /// Run a flow.
    ///
    /// Run a declared flow to its end, or until an action in it needs a person's approval.
    #[effect(returns)]
    RunFlow {
        #[param("The flow, as `app.name` or its id")]
        flow: String,
        #[param("The flow's arguments by parameter name, as a JSON object; resources by id")]
        args: String,
    },
    /// List the catalogue.
    ///
    /// The signatures of every type, action, view and flow of the room's apps, doc comments
    /// included.
    #[effect(kind = "read", returns)]
    Catalogue,
    /// Decide a suggestion.
    ///
    /// Approve or decline what an agent suggested and is waiting on: a suggested invocation
    /// (the id `run-block` or a verb answered with), or a run waiting for approval (the id
    /// `run-flow` answered with). Approving runs the action once, whoever asks and however
    /// many devices answer. Only a person decides: an agent cannot approve what it suggested.
    #[effect(returns, destructive)]
    Resolve {
        #[param("The suggested invocation's id, or the waiting run's id")]
        target: String,
        #[param("`approve` or `decline`")]
        decision: String,
    },
}

/// Showing what a search hit names. Each published resource offers it, bound to itself; the window
/// answers it by opening the app at that resource.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "tilolang.open")]
pub enum OpenEffect {
    /// Open.
    ///
    /// Show this resource in the window: its app, with it selected.
    #[effect(view, row)]
    Open {
        #[param("The resource's id")]
        anchor: String,
    },
}

/// What opening a resource asks of the window: the app to show, and the actions of the app that
/// select the resource in it (a note, a conversation), outermost first.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenPlan {
    pub app: String,
    pub steps: Vec<(String, Vec<tilolang_runtime::Value>)>,
}

fn tree_json(node: &UiNode) -> Json {
    let props: serde_json::Map<String, Json> = node
        .props
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), to_json(value).ok()?)))
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("kind".to_owned(), json!(node.kind));
    out.insert("key".to_owned(), json!(node.key));
    if !props.is_empty() {
        out.insert("props".to_owned(), Json::Object(props));
    }
    if !node.children.is_empty() {
        out.insert(
            "children".to_owned(),
            node.children.iter().map(tree_json).collect(),
        );
    }
    Json::Object(out)
}

fn output(value: Json) -> ActionOutput {
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

impl Host {
    /// How to open the resource `anchor` (an id as [`publish_tree`](crate::publish::publish_tree)
    /// publishes it): its app, and for each item on the way to it the app's `open…` action that
    /// takes the item's parent and the item, which is how a package says an item is selected.
    ///
    /// # Errors
    /// The anchor names nothing in this room.
    pub fn open_plan(&self, anchor: &str) -> Result<OpenPlan, String> {
        use tilolang_runtime::store::Store as _;
        let doc = anchor.split('/').next().unwrap_or_default();
        let (package, _, _) = self
            .records()
            .into_iter()
            .find(|(_, _, id)| id == doc)
            .ok_or_else(|| format!("`{anchor}` is not in this room"))?;
        let meta = &package.meta;
        let root_ty = self
            .store()
            .type_of(&ResourceRef::root(doc, String::new()))
            .map_err(|error| error.to_string())?;
        let parts: Vec<&str> = anchor.split('/').skip(1).collect();
        let mut steps = Vec::new();
        let mut parent = ResourceRef::root(doc, root_ty);
        let mut path = doc.to_owned();
        for part in parts {
            path = format!("{path}/{part}");
            let probe = ResourceRef::parse_id(&path, String::new())
                .ok_or_else(|| format!("`{path}` is not a resource id"))?;
            let ty = self
                .store()
                .type_of(&probe)
                .map_err(|error| error.to_string())?;
            let item = ResourceRef::parse_id(&path, ty.clone())
                .ok_or_else(|| format!("`{path}` is not a resource id"))?;
            let (parent_name, item_name) = (
                meta.types
                    .iter()
                    .find(|t| t.id == parent.ty)
                    .map(|t| t.name.as_str()),
                meta.types
                    .iter()
                    .find(|t| t.id == ty)
                    .map(|t| t.name.as_str()),
            );
            if let Some(action) = meta.actions.iter().find(|action| {
                action.name.starts_with("open")
                    && matches!(
                        action.params.as_slice(),
                        [first, second]
                            if Some(first.ty.trim()) == parent_name
                                && Some(second.ty.trim()) == item_name
                    )
            }) {
                steps.push((
                    action.id.clone(),
                    vec![
                        tilolang_runtime::Value::resource(&parent),
                        tilolang_runtime::Value::resource(&item),
                    ],
                ));
            }
            parent = item;
        }
        Ok(OpenPlan {
            app: meta.app.clone(),
            steps,
        })
    }

    /// Answer `effect` for `caller`, the principal the door admitted (the person at the window
    /// when `None`). An agent's code is written by its own principal, so what it applies to
    /// another app or outside asks a person first.
    ///
    /// # Errors
    /// Why the block, the flow or its arguments were refused, or what the runtime failed with.
    pub fn answer(
        &self,
        effect: HostEffect,
        caller: Option<&Caller>,
    ) -> Result<ActionOutput, String> {
        let agent = &caller.map_or_else(
            || self.viewer().clone(),
            |caller| Principal::new(caller.actor.clone()),
        );
        if let Some(caller) = caller.filter(|caller| caller.agent) {
            self.inner.admit_agent(&caller.actor);
        }
        match effect {
            HostEffect::RunBlock { source } => {
                let block = self
                    .runtime()
                    .run_agent_block(agent, &source)
                    .map_err(|error| error.to_string())?;
                Ok(output(match block {
                    AgentBlock::Rejected(lines) => {
                        return Err(format!("the block does not compile:\n{}", lines.join("\n")));
                    }
                    AgentBlock::View(tree) => json!({"kind": "view", "tree": tree_json(&tree)}),
                    AgentBlock::Data(value) => json!({"kind": "data", "value": value}),
                    AgentBlock::Invocation { record, resource } => json!({
                        "kind": "invocation",
                        "state": record.state,
                        "action": record.name,
                        "app": record.app,
                        "result": record.result,
                        "invocation": resource.id_string(),
                    }),
                }))
            }
            HostEffect::RunFlow { flow, args } => self.run_flow_source(agent, &flow, &args),
            HostEffect::Resolve { target, decision } => {
                if caller.is_some_and(|caller| caller.agent) {
                    return Err("only a person decides what an agent suggested".to_owned());
                }
                self.resolve_suggestion(agent, &target, &decision)
            }
            HostEffect::Catalogue => Ok(output(Json::Array(
                self.runtime()
                    .catalogue()
                    .iter()
                    .filter(|signature| signature.app != tilolang::HOST_APP)
                    .map(|signature| {
                        json!({
                            "app": signature.app,
                            "name": signature.name,
                            "signature": signature.render(),
                        })
                    })
                    .collect(),
            ))),
        }
    }

    fn run_flow_source(
        &self,
        agent: &Principal,
        flow: &str,
        args: &str,
    ) -> Result<ActionOutput, String> {
        let (package, declared) = self
            .packages()
            .into_iter()
            .find_map(|package| {
                let found = package
                    .meta
                    .flows
                    .iter()
                    .find(|declared| {
                        declared.id == flow
                            || declared.name == flow
                            || format!("{}.{}", package.meta.app, declared.name) == flow
                    })
                    .cloned();
                found.map(|declared| (package, declared))
            })
            .ok_or_else(|| format!("no flow `{flow}`"))?;
        let given: serde_json::Map<String, Json> = if args.trim().is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(args)
                .map_err(|error| format!("the arguments are not a JSON object: {error}"))?
        };
        let mut values = Vec::new();
        for param in &declared.params {
            let json = given.get(&param.name).cloned().unwrap_or(Json::Null);
            values.push(value_of(&self.inner, &package.meta, &json, &param.ty)?);
        }
        match self
            .runtime()
            .run_flow(agent, &declared.id, values)
            .map_err(|error| error.to_string())?
        {
            FlowOutcome::Done(value) => Ok(output(
                json!({"state": "done", "result": self.runtime().project(&value)}),
            )),
            FlowOutcome::Suspended(suggestion) => {
                let shown = json!({
                    "state": "suspended",
                    "run": suggestion.run,
                    "waiting_on": suggestion.action,
                });
                self.inner.suspended.borrow_mut().push_back(suggestion);
                Ok(output(shown))
            }
        }
    }

    fn resolve_suggestion(
        &self,
        approver: &Principal,
        target: &str,
        decision: &str,
    ) -> Result<ActionOutput, String> {
        let resolution = match decision.trim() {
            "approve" => Resolution::Approve,
            "decline" => Resolution::Decline,
            other => return Err(format!("`{other}` is not approve or decline")),
        };
        let as_invocation = self
            .invocation_ref(target)
            .filter(|invocation| self.store().contains(&invocation.doc));
        if let Some(invocation) = as_invocation {
            let state = self
                .runtime()
                .resolve_invocation(&invocation, approver, resolution)
                .map_err(|error| error.to_string())?;
            return Ok(output(json!({"kind": "invocation", "state": state})));
        }
        match self
            .runtime()
            .resolve(&target.to_owned(), approver, resolution)
            .map_err(|error| error.to_string())?
        {
            FlowOutcome::Done(value) => Ok(output(
                json!({"kind": "run", "state": "done", "result": self.runtime().project(&value)}),
            )),
            FlowOutcome::Suspended(suggestion) => {
                let shown = json!({
                    "kind": "run",
                    "state": "suspended",
                    "run": suggestion.run,
                    "waiting_on": suggestion.action,
                });
                self.inner.suspended.borrow_mut().push_back(suggestion);
                Ok(output(shown))
            }
        }
    }

    /// The resource a recorded invocation named by `id` is, for a caller that holds its id.
    #[must_use]
    pub fn invocation_ref(&self, id: &str) -> Option<ResourceRef> {
        ResourceRef::parse_id(id, tilolang::INVOCATION_TYPE.to_owned())
    }
}
