//! Action blocks in messages: evaluated once, when the message is
//! posted, and recorded as `Invocation` resources.
//!
//! Rendering never runs an action. An action asks the host, with `run_block`, to evaluate a
//! block after it commits; the host does that under the poster's actor, applies the block's
//! action through the authority (so it runs, or waits for a person's yes), and stores the outcome
//! as an `Invocation` resource that syncs like any other. The embed only draws that resource.
//! A call that waits for a person is asked through the dispatch, which keeps it as its own asked
//! record until they answer; the `Invocation` resource names that call.

use tilolang::{BlockKind, INVOCATION_TYPE};

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::embed::EmbedBlock;
use crate::error::RuntimeError;
use crate::exec::InvokeOutcome;
use crate::handles::QueuedBlock;
use crate::host_res::to_scalar;
use crate::ids::{Actor, AppId, ModuleHash};
use crate::project::{ProjectOptions, project};
use crate::store::{ChangeNote, Mutation};
use crate::value::{ActionRef, ResourceRef, Value, from_json, to_json};

/// What an agent's block came to, as the action API answers it.
#[derive(Debug)]
pub enum AgentBlock {
    /// The source did not compile or link: one line per diagnostic.
    Rejected(Vec<String>),
    /// The block is a view: its rendered tree.
    View(Box<crate::view::UiNode>),
    /// The source is a written view (`tilolang::written`): what it is, and its tree when it
    /// takes nothing and so stands alone.
    Definition {
        info: crate::written::WrittenViewInfo,
        tree: Option<Box<crate::view::UiNode>>,
    },
    /// The block is a value: what an agent sees of it.
    Data(serde_json::Value),
    /// The block applies an action. The host ran it, or asked a person about it, and recorded
    /// the outcome as an `Invocation` resource that syncs like any other.
    Invocation {
        record: Box<InvocationEntry>,
        resource: ResourceRef,
    },
}

/// What the host records of an invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationEntry {
    /// The id of the action.
    pub action: String,
    pub name: String,
    pub app: String,
    /// The arguments as JSON text.
    pub args: String,
    pub author: Actor,
    /// `done`, `asked`, `declined`, `failed` or `denied`.
    pub state: String,
    pub result: String,
    pub run: String,
    /// The resource a call that asks waits on: the one its action names first. `None` for an
    /// action that names no resource, and for one that waits on nobody.
    pub target: Option<ResourceRef>,
}

impl InvocationEntry {
    /// The action's arguments, as it is invoked with them.
    ///
    /// # Errors
    ///
    /// `BadArgs` when the record's arguments do not read as a list of values.
    pub fn arguments(&self) -> Result<Vec<Value>, RuntimeError> {
        let bad = |why: String| RuntimeError::BadArgs(format!("the invocation's arguments: {why}"));
        let json = serde_json::from_str(&self.args).map_err(|e| bad(e.to_string()))?;
        match from_json(&json).map_err(bad)? {
            Value::List(args) => Ok(args),
            _ => Err(bad("not a list".to_owned())),
        }
    }
}

fn field_id(name: &str) -> String {
    format!("inv-{name}")
}

fn note(who: &Actor, identity: &ModuleHash, action: &str) -> ChangeNote {
    ChangeNote {
        action: action.to_owned(),
        args: serde_json::Value::Null,
        invoker: Some(who.clone()),
        chain: vec![identity.0.clone()],
        module: "host".to_owned(),
        ..ChangeNote::default()
    }
}

impl Core {
    /// Evaluate the blocks an action queued, now that it has committed.
    ///
    /// # Errors
    ///
    /// A store failure while recording an invocation.
    pub(crate) fn settle_blocks(
        &self,
        invoker: &Actor,
        queued: Vec<QueuedBlock>,
    ) -> Result<(), RuntimeError> {
        for q in queued {
            self.settle_block(invoker, &q)?;
        }
        Ok(())
    }

    fn settle_block(&self, invoker: &Actor, q: &QueuedBlock) -> Result<(), RuntimeError> {
        // A block that does not compile has nothing to record: the embed shows the diagnostics.
        let Ok(block) = self.embed_block(&q.source) else {
            return Ok(());
        };
        if block.block.kind != BlockKind::Action {
            return Ok(());
        }
        let identity = ModuleHash::written_by(invoker);
        let record = self.run_posted_block(invoker, &identity, &block);
        let inv = self.create_invocation(&record, &note(invoker, &identity, "$invocation"))?;
        self.write_fields(
            &q.part,
            vec![(q.field.clone(), Value::some(Value::resource(&inv)))],
            &note(invoker, &identity, "$settle-block"),
        )
    }

    /// `suggest aref` by `who` where an action or a program runs: the call asks a person
    /// ([`Core::invoke_asking`]), and its outcome is recorded as an `Invocation` resource on the
    /// resource it is about, where whoever sees that resource sees what waits on it.
    ///
    /// # Errors
    ///
    /// A store failure while recording it.
    pub(crate) fn suggest(
        &self,
        who: &Actor,
        aref: &ActionRef,
    ) -> Result<ResourceRef, RuntimeError> {
        let record = self.applied(who, aref, true);
        let identity = ModuleHash::written_by(who);
        self.create_invocation(&record, &note(who, &identity, "$invocation"))
    }

    /// Evaluate the source of a block an agent wrote, as `agent`.
    ///
    /// # Errors
    ///
    /// A store failure while recording an invocation, or a block that traps.
    pub(crate) fn run_agent_block(
        &self,
        agent: &Actor,
        source: &str,
    ) -> Result<AgentBlock, RuntimeError> {
        if tilolang::written::is_declaration(source) {
            let info = match self.check_written_view(source, agent) {
                Ok(info) => info,
                Err(diagnostics) => {
                    return Ok(AgentBlock::Rejected(
                        diagnostics
                            .iter()
                            .map(crate::written::diagnostic_line)
                            .collect(),
                    ));
                }
            };
            let tree = if info.params.is_empty() {
                let rc = self.rc().map_err(RuntimeError::Package)?;
                let view = rc.open_written_view(agent, source, agent, Vec::new())?;
                Some(Box::new(view.tree().clone()))
            } else {
                None
            };
            return Ok(AgentBlock::Definition { info, tree });
        }
        let block = match self.embed_block(source) {
            Ok(block) => block,
            Err(diagnostics) => {
                return Ok(AgentBlock::Rejected(
                    diagnostics
                        .iter()
                        .map(|d| format!("{}:{}: {} {}", d.line, d.col, d.code, d.message))
                        .collect(),
                ));
            }
        };
        let identity = ModuleHash::written_by(agent);
        match block.block.kind {
            BlockKind::View => {
                let target = crate::render::ViewTarget {
                    module: block.pkg.hash.clone(),
                    export: "b_main".to_owned(),
                    view: String::new(),
                    params: Vec::new(),
                };
                let window = crate::view_instance::default_window(&target, &[]);
                let view = crate::view_instance::ViewInstance::open(
                    self.rc().map_err(RuntimeError::Package)?,
                    (agent.clone(), window),
                    target,
                    Vec::new(),
                )?;
                Ok(AgentBlock::View(Box::new(view.tree().clone())))
            }
            BlockKind::Action => {
                let record = self.run_posted_block(agent, &identity, &block);
                let resource =
                    self.create_invocation(&record, &note(agent, &identity, "$invocation"))?;
                Ok(AgentBlock::Invocation {
                    record: Box::new(record),
                    resource,
                })
            }
            BlockKind::Data => {
                let value =
                    self.eval_block(&block.pkg, &block.block.value_type, Some(&identity))?;
                Ok(AgentBlock::Data(project(
                    self,
                    &value,
                    ProjectOptions::default(),
                )))
            }
        }
    }

    /// What a posted action block came to: a `suggest` asks a person even where it may run
    /// without.
    fn run_posted_block(
        &self,
        invoker: &Actor,
        identity: &ModuleHash,
        block: &EmbedBlock,
    ) -> InvocationEntry {
        match self.eval_block(&block.pkg, &block.block.value_type, Some(identity)) {
            Ok(Value::Action(aref)) => self.applied(invoker, &aref, block.block.suggested),
            Ok(_) => self.record_of(
                invoker,
                None,
                "failed",
                "the block did not produce an action",
                "",
            ),
            Err(e) => self.record_of(invoker, None, "failed", &e.to_string(), ""),
        }
    }

    /// The record of `invoker` applying `aref`, asking a person first when `asks`.
    fn applied(&self, invoker: &Actor, aref: &ActionRef, asks: bool) -> InvocationEntry {
        let outcome = if asks {
            self.invoke_asking(invoker, aref)
        } else {
            self.invoke_direct(invoker, aref)
        };
        match outcome {
            Ok(InvokeOutcome::Done(v)) => {
                let shown = self.result_text(&v);
                self.record_of(invoker, Some(aref), "done", &shown, "")
            }
            // The call's own invocation record asks the person: this one names it.
            Ok(InvokeOutcome::Asked(call)) => {
                self.record_of(invoker, Some(aref), "asked", "", &call)
            }
            // It went to its app, which answers it there.
            Ok(InvokeOutcome::Waits { call, .. }) => {
                self.record_of(invoker, Some(aref), "done", "", &call)
            }
            Ok(InvokeOutcome::Denied) => self.record_of(invoker, Some(aref), "denied", "", ""),
            Err(e) => self.record_of(invoker, Some(aref), "failed", &e.to_string(), ""),
        }
    }

    fn result_text(&self, v: &Value) -> String {
        if *v == Value::Unit {
            String::new()
        } else {
            project(self, v, ProjectOptions::default()).to_string()
        }
    }

    fn record_of(
        &self,
        author: &Actor,
        aref: Option<&ActionRef>,
        state: &str,
        result: &str,
        run: &str,
    ) -> InvocationEntry {
        let (action, app, name, args) = match aref {
            Some(a) => {
                let (app, name) = self.action_facts(&a.action);
                let args = to_json(&Value::List(a.args.clone())).unwrap_or_default();
                (a.action.clone(), app, name, args.to_string())
            }
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        InvocationEntry {
            action,
            name,
            app,
            args,
            author: author.clone(),
            state: state.to_owned(),
            result: result.to_owned(),
            run: run.to_owned(),
            // A call that asks waits on the resource its action names first, where whoever sees
            // that resource sees it waiting.
            target: aref.filter(|_| state == "asked").and_then(|a| {
                a.args.iter().find_map(|v| match v {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
            }),
        }
    }

    fn create_invocation(
        &self,
        record: &InvocationEntry,
        note: &ChangeNote,
    ) -> Result<ResourceRef, RuntimeError> {
        let new = self
            .registry
            .borrow()
            .new_record(INVOCATION_TYPE)
            .ok_or_else(|| RuntimeError::NotFound("the host's Invocation type".into()))?;
        let target = self.store.borrow_mut().create(&new)?;
        let text = |name: &str, v: &str| (field_id(name), Value::text(v));
        self.write_fields(
            &target,
            vec![
                text("action", &record.action),
                text("name", &record.name),
                text("app", &record.app),
                text("args", &record.args),
                (field_id("author"), Value::Actor(record.author.clone())),
                text("state", &record.state),
                text("result", &record.result),
                text("run", &record.run),
                (
                    field_id("target"),
                    Value::Opt(record.target.as_ref().map(Value::resource).map(Box::new)),
                ),
            ],
            note,
        )?;
        Ok(target)
    }

    /// Set registers of a resource as one change.
    fn write_fields(
        &self,
        target: &ResourceRef,
        writes: Vec<(String, Value)>,
        note: &ChangeNote,
    ) -> Result<(), RuntimeError> {
        let (_, info) = self
            .registry
            .borrow()
            .type_info(&target.ty)
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", target.ty)))?;
        let mut store = self.store.borrow_mut();
        let txn = store.begin()?;
        for (id, value) in writes {
            let Some(f) = info.field(&id) else {
                store.rollback(txn);
                return Err(RuntimeError::NotFound(format!(
                    "field {id} of {}",
                    info.name
                )));
            };
            let scalar = match to_scalar(&value, &f.ty) {
                Ok(s) => s,
                Err(e) => {
                    store.rollback(txn);
                    return Err(RuntimeError::BadArgs(e));
                }
            };
            let m = Mutation::SetRegister {
                field: id,
                value: scalar,
            };
            if let Err(e) = store.apply(txn, target, &m) {
                store.rollback(txn);
                return Err(e.into());
            }
        }
        store.commit(txn, note)?;
        Ok(())
    }

    /// Read an `Invocation` resource.
    ///
    /// # Errors
    ///
    /// `NotFound` when the resource is not an invocation.
    pub fn read_invocation(&self, inv: &ResourceRef) -> Result<InvocationEntry, RuntimeError> {
        let (_, info) = self
            .registry
            .borrow()
            .type_info(&inv.ty)
            .filter(|(_, info)| info.id == INVOCATION_TYPE)
            .ok_or_else(|| RuntimeError::NotFound("an invocation".into()))?;
        let get = |name: &str| -> Result<Value, RuntimeError> {
            let f = info
                .field(&field_id(name))
                .ok_or_else(|| RuntimeError::NotFound(format!("field {name}")))?;
            self.read_value(inv, f, None).map_err(RuntimeError::Package)
        };
        let text = |name: &str| -> Result<String, RuntimeError> {
            Ok(get(name)?.as_text().unwrap_or_default().to_owned())
        };
        let Value::Actor(author) = get("author")? else {
            return Err(RuntimeError::NotFound("the invocation's author".into()));
        };
        let target = match get("target")? {
            Value::Opt(Some(held)) => match *held {
                Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                _ => None,
            },
            _ => None,
        };
        Ok(InvocationEntry {
            action: text("action")?,
            name: text("name")?,
            app: text("app")?,
            args: text("args")?,
            author,
            state: text("state")?,
            result: text("result")?,
            run: text("run")?,
            target,
        })
    }

    /// The `(resource, field)` pairs an invocation card reads, for a view's read recorder.
    pub(crate) fn invocation_reads(&self, inv: &ResourceRef) -> Vec<(String, String)> {
        let Some((_, info)) = self.registry.borrow().type_info(&inv.ty) else {
            return Vec::new();
        };
        info.fields
            .iter()
            .map(|f| (inv.id_string(), f.id.clone()))
            .collect()
    }

    /// What the authority needs to decide about an action by id.
    pub(crate) fn info_of(&self, action: &str) -> Option<ActionInfo> {
        let reg = self.registry.borrow();
        let pkg = reg.action_package(action)?;
        let class = pkg.action(action)?.class;
        Some(ActionInfo {
            action: action.to_owned(),
            app: AppId::from(pkg.meta.app.as_str()),
            class,
            own: true,
            module: pkg.hash.clone(),
            target: None,
            admitted: false,
        })
    }

    /// Whether `who` may run this action at all: the authority does not deny them, though it may
    /// ask them first. It is what a view offers them: an action their state denies stands
    /// disabled. Their state is theirs alone, so a view that asked runs again when it changes.
    pub(crate) fn may_decide(&self, who: &Actor, action: &str) -> bool {
        self.read_person();
        self.info_of(action)
            .is_none_or(|info| self.authority.decide(who, &[], &info) != Decision::Deny)
    }

    /// Record what became of the call `call` that asked a person, now that it was answered: each
    /// `Invocation` resource on a resource of `about` that names it while it waits says `state`
    /// from then on, so what waits on a resource leaves it once answered.
    ///
    /// # Errors
    ///
    /// A store failure while reading or writing one.
    pub(crate) fn call_answered(
        &self,
        call: &str,
        about: &[ResourceRef],
        state: &str,
    ) -> Result<(), RuntimeError> {
        for of in about {
            let links = self.store.borrow().linked_by(of, INVOCATION_TYPE)?;
            for at in links.into_iter().map(|link| link.of) {
                let rec = self.read_invocation(&at)?;
                if rec.state == "asked" && rec.run == call {
                    let identity = ModuleHash::written_by(&rec.author);
                    self.write_fields(
                        &at,
                        vec![(field_id("state"), Value::text(state))],
                        &note(&rec.author, &identity, "$invocation"),
                    )?;
                }
            }
        }
        Ok(())
    }
}
