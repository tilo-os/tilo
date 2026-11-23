//! Action blocks in messages: evaluated once, when the message is
//! posted, and recorded as `Invocation` resources.
//!
//! Rendering never runs an action. An action asks the host, with `run_block`, to evaluate a
//! block after it commits; the host does that under the poster's actor, applies the block's
//! action through the authority (so it runs, or becomes a suggestion), and stores the outcome
//! as an `Invocation` resource that syncs like any other. The embed only draws that resource.
//! A suggestion is decided later, by any replica, with [`Core::resolve_invocation`].

use tilolang::{BlockKind, INVOCATION_TYPE};

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::embed::EmbedBlock;
use crate::error::RuntimeError;
use crate::exec::{ActionOutcome, ExecCtx, InvokeOutcome};
use crate::handles::QueuedBlock;
use crate::host_res::to_scalar;
use crate::ids::{Actor, AppId, ModuleHash};
use crate::project::{ProjectOptions, project};
use crate::run::Resolution;
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
    /// The block applies an action. The host ran it, or suggested it to a person, and recorded
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
    /// The resource a suggestion waits on: the one its action names first. `None` for an
    /// action that names no resource, and for one that waits on nobody.
    pub target: Option<ResourceRef>,
    /// Whether this is a proposal a person decides, rather than one the authority asked about
    /// as the action was applied: accepting one runs it as the accepting person, attributed to
    /// whoever proposed it.
    pub proposed: bool,
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

fn note(
    who: &Actor,
    identity: &ModuleHash,
    action: &str,
    approved_by: Option<&Actor>,
) -> ChangeNote {
    ChangeNote {
        action: action.to_owned(),
        args: serde_json::Value::Null,
        invoker: Some(who.clone()),
        chain: vec![identity.0.clone()],
        module: "host".to_owned(),
        approved_by: approved_by.cloned(),
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
        let (record, made) = self.run_posted_block(invoker, &identity, &block);
        let inv = match made {
            Some(inv) => inv,
            None => {
                self.create_invocation(&record, &note(invoker, &identity, "$invocation", None))?
            }
        };
        self.write_fields(
            &q.part,
            vec![(q.field.clone(), Value::some(Value::resource(&inv)))],
            &note(invoker, &identity, "$settle-block", None),
        )
    }

    /// Record `aref` as a proposal of `proposer`: an action that waits on the resource it is
    /// about instead of being applied, attached to that resource as an ordinary change anyone
    /// who sees the resource sees, until a person accepts or dismisses it.
    ///
    /// # Errors
    ///
    /// A store failure while recording it.
    pub(crate) fn propose(
        &self,
        proposer: &Actor,
        aref: &ActionRef,
    ) -> Result<ResourceRef, RuntimeError> {
        let aref = &self.completed(aref)?;
        let mut record = self.record_of(proposer, Some(aref), "asked", "", "");
        record.proposed = true;
        let identity = ModuleHash::written_by(proposer);
        self.create_invocation(&record, &note(proposer, &identity, "$suggest", None))
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
                let (record, made) = self.run_posted_block(agent, &identity, &block);
                let resource = match made {
                    Some(made) => made,
                    None => self
                        .create_invocation(&record, &note(agent, &identity, "$invocation", None))?,
                };
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

    /// What a posted action block came to, and the record the host already wrote for it when
    /// that is the block's own (`None` for an outcome the caller records).
    fn run_posted_block(
        &self,
        invoker: &Actor,
        identity: &ModuleHash,
        block: &EmbedBlock,
    ) -> (InvocationEntry, Option<ResourceRef>) {
        let value = self.eval_block(&block.pkg, &block.block.value_type, Some(identity));
        let aref = match value {
            Ok(Value::Action(aref)) => aref,
            Ok(_) => {
                return (
                    self.record_of(
                        invoker,
                        None,
                        "failed",
                        "the block did not produce an action",
                        "",
                    ),
                    None,
                );
            }
            Err(e) => {
                return (
                    self.record_of(invoker, None, "failed", &e.to_string(), ""),
                    None,
                );
            }
        };
        // A proposal is recorded as it stands: the record the message card draws is the one a
        // person decides, so accepting it there applies it.
        if block.block.suggested {
            let proposed = self
                .propose(invoker, &aref)
                .and_then(|inv| Ok((self.read_invocation(&inv)?, inv)));
            return match proposed {
                Ok((record, inv)) => (record, Some(inv)),
                Err(e) => (
                    self.record_of(invoker, Some(&aref), "failed", &e.to_string(), ""),
                    None,
                ),
            };
        }
        let record = match self.invoke_direct(invoker, &aref) {
            Ok(InvokeOutcome::Done(v)) => {
                let shown = self.result_text(&v);
                self.record_of(invoker, Some(&aref), "done", &shown, "")
            }
            // The call's own invocation record asks the person: this one names it.
            Ok(InvokeOutcome::Asked(call)) => {
                self.record_of(invoker, Some(&aref), "asked", "", &call)
            }
            // It went to its app, which answers it there.
            Ok(InvokeOutcome::Waits { call, .. }) => {
                self.record_of(invoker, Some(&aref), "done", "", &call)
            }
            Ok(InvokeOutcome::Denied) => self.record_of(invoker, Some(&aref), "denied", "", ""),
            Err(e) => self.record_of(invoker, Some(&aref), "failed", &e.to_string(), ""),
        };
        (record, None)
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
                let (app, name, _) = self.action_facts(&a.action, &a.args);
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
            // A suggestion waits on the resource its action names first, where a person finds
            // it and decides it, whether someone proposed it or the authority asked about it.
            target: aref.filter(|_| state == "asked").and_then(|a| {
                a.args.iter().find_map(|v| match v {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
            }),
            proposed: false,
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
                (field_id("proposed"), Value::Bool(record.proposed)),
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
            proposed: matches!(get("proposed")?, Value::Bool(true)),
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
        })
    }

    /// Whether `who` may decide a suggestion of this action: the authority does not deny them.
    /// An `ask` is the very decision they are making, so only a denial refuses. It is what a view
    /// offers them too: an action their state denies stands disabled. Their state is theirs
    /// alone, so a view that asked runs again when it changes.
    pub(crate) fn may_decide(&self, who: &Actor, action: &str) -> bool {
        self.read_person();
        self.info_of(action)
            .is_none_or(|info| self.authority.decide(who, &[], &info) != Decision::Deny)
    }

    /// Decide the suggestion an invocation resource holds, as `approver`, on this replica.
    /// Approving runs the action once with `approver` as its invoker: a proposal runs with the
    /// authority of the person accepting it and records in its change who proposed it, while
    /// an ask the authority raised runs for the person who asked. The outcome is written back
    /// to the resource, so every replica shows it after sync. Declining only records the
    /// refusal.
    ///
    /// # Errors
    ///
    /// `NotPending` when the invocation is not a suggestion; `Denied` when the authority
    /// gives `approver` no say; otherwise as running the action.
    pub fn resolve_invocation(
        &self,
        inv: &ResourceRef,
        approver: &Actor,
        resolution: Resolution,
    ) -> Result<String, RuntimeError> {
        let rec = self.read_invocation(inv)?;
        // Only a proposal is decided here: a call that asked waits on its own invocation record,
        // which the dispatch answers.
        if rec.state != "asked" || !rec.proposed {
            return Err(RuntimeError::NotPending(inv.id_string()));
        }
        if !self.may_decide(approver, &rec.action) {
            return Err(RuntimeError::Denied(format!(
                "{approver} may not decide `{}`",
                rec.name
            )));
        }
        let identity = ModuleHash::written_by(&rec.author);
        let (state, result, run) = match resolution {
            Resolution::Decline => (
                "declined".to_owned(),
                format!("declined by {approver}"),
                String::new(),
            ),
            Resolution::Approve { .. } | Resolution::ApproveAndGrant => {
                self.approve_invocation(&inv.id_string(), &rec, &identity, approver)?
            }
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
        origin: &str,
        rec: &InvocationEntry,
        identity: &ModuleHash,
        approver: &Actor,
    ) -> Result<(String, String, String), RuntimeError> {
        // The effects the accepted proposal queues are keyed by the decision, which every
        // replica reads.
        let run = format!("invocation:{origin}");
        let args = rec.arguments()?;
        // A proposal runs as the person accepting it, and its change says who proposed it.
        let (invoker, suggested_by) = (approver.clone(), Some(rec.author.clone()));
        let ctx = ExecCtx {
            invoker,
            chain: vec![identity.clone()],
            approved_by: Some(approver.clone()),
            suggested_by,
            identity: Some(run),
            from: None,
            at: None,
            gesture: None,
        };
        let aref = ActionRef {
            action: rec.action.clone(),
            args,
            author: Some(identity.clone()),
            from: None,
        };
        Ok(match self.run_action(&ctx, &aref)? {
            ActionOutcome::Done(v) => ("done".to_owned(), self.result_text(&v), String::new()),
            ActionOutcome::Waits(_) => ("approved".to_owned(), String::new(), String::new()),
            ActionOutcome::Failed(why) => ("failed".to_owned(), why, String::new()),
        })
    }
}
