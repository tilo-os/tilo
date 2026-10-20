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
use tilolang_runtime::{AgentBlock, FlowOutcome, Principal, UiNode};

use crate::host::{Host, value_of};

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
    /// Answer `effect` for `agent`, whose code a block's author is.
    ///
    /// # Errors
    /// Why the block, the flow or its arguments were refused, or what the runtime failed with.
    pub fn answer(&self, effect: HostEffect, agent: &Principal) -> Result<ActionOutput, String> {
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

    /// The resource a recorded invocation named by `id` is, for a caller that holds its id.
    #[must_use]
    pub fn invocation_ref(&self, id: &str) -> Option<ResourceRef> {
        ResourceRef::parse_id(id, tilolang::INVOCATION_TYPE.to_owned())
    }
}
