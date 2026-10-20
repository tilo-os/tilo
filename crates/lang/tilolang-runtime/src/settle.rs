//! Action blocks in messages (implementation design 7.8): evaluated once, when the message is
//! posted, and recorded as `Invocation` resources.
//!
//! Rendering never runs an action. An action asks the host, with `run_block`, to evaluate a
//! block after it commits; the host does that under the poster's principal, applies the block's
//! action through the authority (so it runs, or becomes a suggestion), and stores the outcome
//! as an `Invocation` resource that syncs like any other. The embed only draws that resource.
//! A suggestion is decided later, by any replica, with [`Core::resolve_invocation`].

use tilolang::{BlockKind, INVOCATION_TYPE};

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::embed::EmbedBlock;
use crate::error::RuntimeError;
use crate::exec::{ActionOutcome, ExecCtx, InvokeOutcome};
use crate::flow::{FlowOutcome, Resolution};
use crate::handles::QueuedBlock;
use crate::host_res::to_scalar;
use crate::ids::{ModuleHash, Principal};
use crate::project::{ProjectOptions, project};
use crate::store::{ChangeNote, Mutation};
use crate::value::{ActionRef, ResourceRef, Value, from_json, to_json};

/// What an agent's block came to, as the door answers it.
#[derive(Debug)]
pub enum AgentBlock {
    /// The source did not compile or link: one line per diagnostic.
    Rejected(Vec<String>),
    /// The block is a view: its rendered tree.
    View(Box<crate::view::UiNode>),
    /// The block is a value: what an agent sees of it.
    Data(serde_json::Value),
    /// The block applies an action. The host ran it, or suggested it to a person, and recorded
    /// the outcome as an `Invocation` resource that syncs like any other.
    Invocation {
        record: InvocationRecord,
        resource: ResourceRef,
    },
}

/// What the host records of an invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationRecord {
    /// The id of the action or flow.
    pub action: String,
    pub name: String,
    pub app: String,
    /// The arguments as JSON text.
    pub args: String,
    pub author: Principal,
    /// `done`, `suggested`, `declined`, `failed` or `denied`.
    pub state: String,
    pub result: String,
    pub run: String,
}

fn field_id(name: &str) -> String {
    format!("inv-{name}")
}

fn note(
    who: &Principal,
    identity: &ModuleHash,
    action: &str,
    approved_by: Option<&Principal>,
) -> ChangeNote {
    ChangeNote {
        action: action.to_owned(),
        args: serde_json::Value::Null,
        invoker: who.0.clone(),
        chain: vec![identity.0.clone()],
        module: "host".to_owned(),
        approved_by: approved_by.map(|p| p.0.clone()),
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
        invoker: &Principal,
        queued: Vec<QueuedBlock>,
    ) -> Result<(), RuntimeError> {
        for q in queued {
            self.settle_block(invoker, &q)?;
        }
        Ok(())
    }

    fn settle_block(&self, invoker: &Principal, q: &QueuedBlock) -> Result<(), RuntimeError> {
        // A block that does not compile has nothing to record: the embed shows the diagnostics.
        let Ok(block) = self.embed_block(&q.source) else {
            return Ok(());
        };
        if block.block.kind != BlockKind::Action {
            return Ok(());
        }
        let identity = ModuleHash::principal(&invoker.0);
        let record = self.run_posted_block(invoker, &identity, &block);
        let inv =
            self.create_invocation(&record, &note(invoker, &identity, "$invocation", None))?;
        self.write_fields(
            &q.part,
            vec![(q.field.clone(), Value::some(Value::resource(&inv)))],
            &note(invoker, &identity, "$settle-block", None),
        )
    }

    /// Evaluate the source of a block an agent wrote, as `agent` (implementation design 6.1).
    ///
    /// # Errors
    ///
    /// A store failure while recording an invocation, or a block that traps.
    pub(crate) fn run_agent_block(
        &self,
        agent: &Principal,
        source: &str,
    ) -> Result<AgentBlock, RuntimeError> {
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
        let identity = ModuleHash::principal(&agent.0);
        match block.block.kind {
            BlockKind::View => {
                let target = crate::render::ViewTarget {
                    module: block.pkg.hash.clone(),
                    export: "b_main".to_owned(),
                    params: Vec::new(),
                };
                let view = crate::view_instance::ViewInstance::open(
                    self.rc().map_err(RuntimeError::Package)?,
                    agent.clone(),
                    target,
                    Vec::new(),
                )?;
                Ok(AgentBlock::View(Box::new(view.tree().clone())))
            }
            BlockKind::Action => {
                let record = self.run_posted_block(agent, &identity, &block);
                let resource =
                    self.create_invocation(&record, &note(agent, &identity, "$invocation", None))?;
                Ok(AgentBlock::Invocation { record, resource })
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

    fn run_posted_block(
        &self,
        invoker: &Principal,
        identity: &ModuleHash,
        block: &EmbedBlock,
    ) -> InvocationRecord {
        let value = self.eval_block(&block.pkg, &block.block.value_type, Some(identity));
        let aref = match value {
            Ok(Value::Action(aref)) => aref,
            Ok(_) => {
                return self.record_of(
                    invoker,
                    None,
                    "failed",
                    "the block did not produce an action",
                    "",
                );
            }
            Err(e) => return self.record_of(invoker, None, "failed", &e.to_string(), ""),
        };
        match self.invoke_direct(invoker, &aref) {
            Ok(InvokeOutcome::Done(v)) => {
                let shown = self.result_text(&v);
                self.record_of(invoker, Some(&aref), "done", &shown, "")
            }
            Ok(InvokeOutcome::Suggested(s)) => {
                self.record_of(invoker, Some(&aref), "suggested", "", &s.run)
            }
            Ok(InvokeOutcome::Denied) => self.record_of(invoker, Some(&aref), "denied", "", ""),
            Err(e) => self.record_of(invoker, Some(&aref), "failed", &e.to_string(), ""),
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
        author: &Principal,
        aref: Option<&ActionRef>,
        state: &str,
        result: &str,
        run: &str,
    ) -> InvocationRecord {
        let (action, app, name, args) = match aref {
            Some(a) => {
                let (app, name, _) = self.action_facts(&a.action, &a.args);
                let args = to_json(&Value::List(a.args.clone())).unwrap_or_default();
                (a.action.clone(), app, name, args.to_string())
            }
            None => (String::new(), String::new(), String::new(), String::new()),
        };
        InvocationRecord {
            action,
            name,
            app,
            args,
            author: author.clone(),
            state: state.to_owned(),
            result: result.to_owned(),
            run: run.to_owned(),
        }
    }

    fn create_invocation(
        &self,
        record: &InvocationRecord,
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
                (field_id("author"), Value::Principal(record.author.clone())),
                text("state", &record.state),
                text("result", &record.result),
                text("run", &record.run),
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
    pub fn read_invocation(&self, inv: &ResourceRef) -> Result<InvocationRecord, RuntimeError> {
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
        let author = match get("author")? {
            Value::Principal(p) => p,
            _ => Principal::new(""),
        };
        Ok(InvocationRecord {
            action: text("action")?,
            name: text("name")?,
            app: text("app")?,
            args: text("args")?,
            author,
            state: text("state")?,
            result: text("result")?,
            run: text("run")?,
        })
    }

    /// The `(document, field)` pairs an invocation card reads, for a view's read recorder.
    pub(crate) fn invocation_reads(&self, inv: &ResourceRef) -> Vec<(String, String)> {
        let Some((_, info)) = self.registry.borrow().type_info(&inv.ty) else {
            return Vec::new();
        };
        info.fields
            .iter()
            .map(|f| (inv.doc.clone(), f.id.clone()))
            .collect()
    }

    /// What the authority needs to decide about an action or flow by id.
    pub(crate) fn info_of(&self, action: &str) -> Option<ActionInfo> {
        let reg = self.registry.borrow();
        if let Some(pkg) = reg.action_package(action) {
            let class = pkg.action(action)?.class;
            return Some(ActionInfo {
                action: action.to_owned(),
                app: pkg.meta.app.clone(),
                class,
                own: true,
                module: pkg.hash.clone(),
            });
        }
        let pkg = reg.flow_package(action)?;
        let class = pkg.flow(action)?.class;
        Some(ActionInfo {
            action: action.to_owned(),
            app: pkg.meta.app.clone(),
            class,
            own: true,
            module: pkg.hash.clone(),
        })
    }

    /// Whether `who` may decide a suggestion of this action: the authority does not deny them.
    /// An `ask` is the very decision they are making, so only a denial refuses.
    pub(crate) fn may_decide(&self, who: &Principal, action: &str) -> bool {
        self.info_of(action)
            .is_none_or(|info| self.authority.decide(who, &[], &info) != Decision::Deny)
    }

    /// Decide the suggestion an invocation resource holds, as `approver`, on this replica.
    /// Approving runs the action once with the poster as its invoker and the poster's
    /// authority as its chain; the outcome is written back to the resource, so every replica
    /// shows it after sync. Declining only records the refusal.
    ///
    /// # Errors
    ///
    /// `NotPending` when the invocation is not a suggestion; `Denied` when the authority
    /// gives `approver` no say; otherwise as running the action.
    pub fn resolve_invocation(
        &self,
        inv: &ResourceRef,
        approver: &Principal,
        resolution: Resolution,
    ) -> Result<String, RuntimeError> {
        let rec = self.read_invocation(inv)?;
        if rec.state != "suggested" {
            return Err(RuntimeError::NotPending(inv.id_string()));
        }
        if !self.may_decide(approver, &rec.action) {
            return Err(RuntimeError::Denied(format!(
                "{approver} may not decide `{}`",
                rec.name
            )));
        }
        let identity = ModuleHash::principal(&rec.author.0);
        let (state, result, run) = match resolution {
            Resolution::Decline => (
                "declined".to_owned(),
                format!("declined by {approver}"),
                String::new(),
            ),
            Resolution::Approve => self.approve_invocation(&rec, &identity, approver)?,
        };
        let nt = note(approver, &identity, "$decide-invocation", Some(approver));
        self.write_fields(
            inv,
            vec![
                (field_id("state"), Value::text(&state)),
                (field_id("result"), Value::text(result)),
                (field_id("run"), Value::text(run)),
            ],
            &nt,
        )?;
        Ok(state)
    }

    fn approve_invocation(
        &self,
        rec: &InvocationRecord,
        identity: &ModuleHash,
        approver: &Principal,
    ) -> Result<(String, String, String), RuntimeError> {
        let parsed: serde_json::Value = serde_json::from_str(&rec.args)
            .map_err(|e| RuntimeError::BadArgs(format!("the invocation's arguments: {e}")))?;
        let Value::List(args) = from_json(&parsed).map_err(RuntimeError::BadArgs)? else {
            return Err(RuntimeError::BadArgs("the invocation's arguments".into()));
        };
        if self.registry.borrow().flow_package(&rec.action).is_some() {
            return Ok(
                match self.start_flow(&rec.author, &rec.action, args, vec![identity.clone()])? {
                    FlowOutcome::Done(v) => {
                        ("done".to_owned(), self.result_text(&v), String::new())
                    }
                    FlowOutcome::Suspended(s) => ("suggested".to_owned(), String::new(), s.run),
                },
            );
        }
        let ctx = ExecCtx {
            invoker: rec.author.clone(),
            chain: vec![identity.clone()],
            approved_by: Some(approver.clone()),
        };
        let aref = ActionRef {
            action: rec.action.clone(),
            args,
            author: Some(identity.clone()),
        };
        Ok(match self.run_action(&ctx, &aref)? {
            ActionOutcome::Done(v) => ("done".to_owned(), self.result_text(&v), String::new()),
            ActionOutcome::Failed(why) => ("failed".to_owned(), why, String::new()),
        })
    }
}
