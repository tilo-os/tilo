//! An agent task's invocation applied at the origin of one document, with no window: the
//! document on its connection, the app's package in a runtime of its own, and the answer sent
//! back once the workspace holds what the action wrote.

use super::{
    ACTION_TYPE, ActionAddress, ActionContext, ActionInvoker, Bundle, CALLER, Caller,
    DocumentScope, DynAction, EDIT_TYPE, HistoryEdit, Host, InstanceDocument, MemberId, Phrases,
    Rc, Spec, Surface, Waker, package_resource_id, returned, window_phrases,
};
use tilolang_tilo::meta::MetaExt as _;

/// What an [`Applier`] has done with its change so far.
enum Applying {
    /// The change has not arrived on its connection yet.
    Awaiting(Vec<u8>),
    /// It arrived; it runs once what the document publishes is built.
    Arrived(Box<runtime_portal::process_link::Change>),
    /// It ran: its answer, sent once the workspace was handed what it wrote.
    Ran(serde_json::Value),
    /// The answer was sent.
    Answered,
}

/// The origin of one document applying one change: an agent task's invocation the broker
/// admitted (`docs/designs/app-language/agents.md`, "Reading documents outside their app"). No
/// window and no instance: the document on its connection, which holds the app's ownership as an
/// instance's does, and the app's package in a runtime of its own. The action's code runs
/// against the document as it is when the change is applied, so its requirements are decided on
/// what others wrote since the task read it; what it wrote is applied as the invoker's step
/// and handed to the workspace, and the answer says how it differed from the task's preview.
pub struct Applier {
    spec: Spec,
    record: InstanceDocument,
    host: Host,
    surface: Rc<Surface>,
    /// The app's resource, by its id and kind: where its history verbs are published.
    resource: (String, String),
    connection: std::os::unix::net::UnixStream,
    applying: Applying,
    /// Whether the answer carries the whole document: the root this change needed was made for
    /// it.
    whole: bool,
    /// The scope the change's code runs in, the document's content store registered for it.
    scope: DocumentScope,
}

impl Applier {
    /// The origin of `document` for `bundle`'s app, taking its change on `connection`.
    pub fn open(
        spec: Spec,
        record: InstanceDocument,
        bundle: &Bundle,
        phrases: &Phrases,
        wake: Waker,
        connection: std::os::fd::OwnedFd,
    ) -> Result<Self, String> {
        let mut record = record;
        let scope = DocumentScope::of(&record, &wake);
        let _code = scope.enter();
        // The host opens without the document, which its store holds from here on: what the
        // origin applies is written there, and read there at once.
        let host = Host::open(
            tilolang_tilo::host::Opening {
                ..tilolang_tilo::host::Opening::default()
            },
            tilolang_tilo::runtime::Actor::Member(MemberId::person(spec.person.clone())),
            || None,
        )
        .map_err(|error| error.to_string())?;
        record.hold_in(&host);
        host.set_clock(tilolang_tilo::host::system_time);
        host.load_bundle(bundle)
            .map_err(|error| error.to_string())?;
        // A change made to the app's document at its origin reads in the language the app's window
        // would read in: the person's, not this host process's.
        let phrases = window_phrases(phrases, &spec, bundle);
        let activate = tilo_actions::remote::activate_spec(
            phrases.say("app-activate-window", &[]),
            phrases.say("app-activate-window-description", &[]),
        );
        let surface = Rc::new(Surface::new(&bundle.meta.app, activate, &host, wake));
        let connection = std::os::unix::net::UnixStream::from(connection);
        connection
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            resource: (
                package_resource_id(&bundle.meta.app),
                bundle.meta.package_kind(),
            ),
            spec,
            record,
            host,
            surface,
            connection,
            applying: Applying::Awaiting(Vec::new()),
            whole: false,
            scope,
        })
    }

    /// What the applier is.
    #[must_use]
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// The document it changes, as it holds it.
    #[must_use]
    pub fn record(&self) -> &InstanceDocument {
        &self.record
    }

    /// The change's connection, to wait on.
    pub fn connection(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.connection.as_fd()
    }

    /// Whether the change was answered: the applier has nothing left to do.
    #[must_use]
    pub fn answered(&self) -> bool {
        matches!(self.applying, Applying::Answered)
    }

    /// Whether it has work of its own in hand: a change that arrived and has not run, or one still
    /// to answer. A change its connection has not carried yet is the session's, not its own.
    #[must_use]
    pub fn owes(&self) -> bool {
        matches!(self.applying, Applying::Arrived(_) | Applying::Ran(_))
    }

    /// One step: take in what the workspace sent and what came on the connection, run the change
    /// once it can, and answer once the workspace was handed what it wrote. Whether anything changed.
    pub fn turn(&mut self) -> bool {
        let _code = self.scope.enter();
        let mut changed = self.record.take_in_document();
        self.surface.update();
        let state = std::mem::replace(&mut self.applying, Applying::Answered);
        self.applying = match state {
            Applying::Awaiting(mut read) => match self.read_change(&mut read) {
                Ok(Some(change)) => {
                    changed = true;
                    Applying::Arrived(Box::new(change))
                }
                Ok(None) => Applying::Awaiting(read),
                Err(why) => {
                    changed = true;
                    Applying::Ran(serde_json::json!({ "failed": why }))
                }
            },
            Applying::Arrived(change) if self.surface.publications.borrow().is_building() => {
                Applying::Arrived(change)
            }
            Applying::Arrived(change) => {
                changed = true;
                match self.make_root(&change) {
                    // The root it made publishes first; the change resolves against it then.
                    Ok(true) => Applying::Arrived(change),
                    Ok(false) => Applying::Ran(self.apply(&change)),
                    Err(why) => Applying::Ran(serde_json::json!({ "failed": why })),
                }
            }
            other => other,
        };
        if let Applying::Ran(answer) = &self.applying
            && (self.record.share().is_some() || !self.record.unsent())
        {
            use std::io::Write as _;
            let mut line = answer.to_string().into_bytes();
            line.push(b'\n');
            if let Err(error) = (&self.connection).write_all(&line) {
                tracing::warn!(app = self.spec.app, %error, "a change's answer did not reach the session");
            }
            self.applying = Applying::Answered;
            changed = true;
        }
        changed
    }

    /// The change, once its whole line arrived on the connection.
    fn read_change(
        &self,
        read: &mut Vec<u8>,
    ) -> Result<Option<runtime_portal::process_link::Change>, String> {
        use std::io::Read as _;
        let mut chunk = [0_u8; 4096];
        loop {
            match (&self.connection).read(&mut chunk) {
                Ok(0) => return Err("the session left before it said the change".to_owned()),
                Ok(n) => read.extend_from_slice(chunk.get(..n).unwrap_or_default()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("the change's connection failed: {error}")),
            }
        }
        let Some(end) = read.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let line = read.get(..end).unwrap_or_default();
        serde_json::from_slice(line)
            .map(Some)
            .map_err(|error| format!("the session said a change that does not decode: {error}"))
    }

    /// Make the app's root when `change` addresses it and the document does not hold it yet: the
    /// one per-workspace resource of each of the package's root types is made the first time it
    /// is wanted, here as in a window that opens it, and a task reads it from the package
    /// whether or not any window ever made it. A package may declare several roots
    /// (`root viewing: Viewing`, `root Shell`); the change names which. It is applied as the
    /// change's invoker's step, before the change. Whether it made it.
    fn make_root(&mut self, change: &runtime_portal::process_link::Change) -> Result<bool, String> {
        let prefix = tilolang_tilo::publish::resource_id(&self.spec.app, "");
        // The resource's document: an item's path starts with it.
        let Some(doc) = change
            .resource
            .strip_prefix(&prefix)
            .and_then(|path| path.split('/').next())
        else {
            return Ok(false);
        };
        let Some(ty) = doc.strip_prefix("root-") else {
            return Ok(false);
        };
        if self.host.store().contains(doc) {
            return Ok(false);
        }
        let declared = self
            .host
            .package(&self.spec.app)
            .is_some_and(|package| package.declares_root(ty));
        if !declared {
            return Err(format!(
                "{} is no root of {}",
                change.resource, self.spec.app
            ));
        }
        let invoker: ActionInvoker = serde_json::from_value(change.invoker.clone())
            .map_err(|error| format!("the change's invoker does not decode: {error}"))?;
        // Made as the invoker's step: the store writes it as theirs.
        self.host
            .store()
            .writing_as(&invoker.acting.by, || {
                self.host.root_resource(&self.spec.app, ty)
            })
            .map_err(|error| error.to_string())?;
        let ambient: Option<ActionContext> = change
            .context
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| format!("the change's context does not decode: {error}"))?;
        let ambient = ambient.map(|context| context.with_invoker(invoker.clone()));
        // A document without its root is one the task read as none, or as what an app's window
        // never shared: the answer carries the whole document, which is small then.
        self.whole = true;
        for committed in self.host.take_committed() {
            self.record
                .apply_change(&committed.action, &invoker, ambient.as_ref())
                .map_err(|error| error.to_string())?;
        }
        self.surface.update();
        Ok(true)
    }

    /// Run `change` against the document as it is now and apply what it wrote: its answer.
    fn apply(&mut self, change: &runtime_portal::process_link::Change) -> serde_json::Value {
        let invoker: ActionInvoker = match serde_json::from_value(change.invoker.clone()) {
            Ok(invoker) => invoker,
            Err(error) => {
                return serde_json::json!({"failed": format!("the change's invoker does not decode: {error}")});
            }
        };
        let ambient: Option<ActionContext> = match change
            .context
            .clone()
            .map(serde_json::from_value)
            .transpose()
        {
            Ok(ambient) => ambient,
            Err(error) => {
                return serde_json::json!({"failed": format!("the change's context does not decode: {error}")});
            }
        };
        let address = ActionAddress::new(change.resource.clone(), change.action.clone());
        // An agent asks of the resource it names, whatever a window shows: the workspace is not
        // published for one change, and an applier shows nothing.
        let resources = self.surface.addressed(&[], &address.resource);
        let resources = self.surface.naming(resources, &address);
        let caller = Caller::from(&invoker);
        // The action is applied as soon as it is built: the run that builds it is the one whose
        // writes the document commits, as the invoker's.
        CALLER.with(|slot| *slot.borrow_mut() = Some(caller));
        let encoded = self.host.carrying_out(&invoker.acting.by, || {
            tilo_actions::api::encode_json(&resources, &address, &change.arguments)
        });
        CALLER.with(|slot| *slot.borrow_mut() = None);
        let encoded = match encoded {
            Ok(encoded) if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE => {
                return serde_json::json!({"failed": String::from_utf8_lossy(&encoded.payload)});
            }
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::warn!(
                    app = self.spec.app,
                    resource = address.resource,
                    action = address.action,
                    offered = resources.len(),
                    %error,
                    "the document's origin does not offer the change it was handed"
                );
                return serde_json::json!({"failed": error.to_string()});
            }
        };
        let ambient = ambient.map(|context| context.with_invoker(invoker.clone()));
        if encoded.type_id == EDIT_TYPE {
            let edit: HistoryEdit = match serde_json::from_slice(&encoded.payload) {
                Ok(edit) => edit,
                Err(error) => return serde_json::json!({"failed": error.to_string()}),
            };
            let before = self.record.marks();
            return match self.record.history_at_origin(
                &edit,
                &address.resource,
                &self.resource.1,
                invoker,
                ambient.as_ref(),
            ) {
                Ok(value) => {
                    // What it wrote, for the task to read back at once, as a change's answer.
                    let wrote = self.wrote_since(&before);
                    serde_json::json!({"ok": {"value": value, "resources": []}, "change": wrote})
                }
                Err(error) => serde_json::json!({"failed": error.to_string()}),
            };
        }
        if encoded.type_id != ACTION_TYPE {
            return serde_json::json!({"failed": format!(
                "{} is no change of the document",
                address.action
            )});
        }
        let action: DynAction = match serde_json::from_slice(&encoded.payload) {
            Ok(action) => action,
            Err(error) => return serde_json::json!({"failed": error.to_string()}),
        };
        // What it wrote includes the root it made for it, which the task's read lacks.
        let before = if std::mem::take(&mut self.whole) {
            tilolang_tilo::chunk::Stamps::new()
        } else {
            self.record.marks()
        };
        if let Err(error) = self
            .record
            .apply_change(&action, &invoker, ambient.as_ref())
        {
            return serde_json::json!({"failed": error.to_string()});
        }
        let output = returned(&self.host, &action);
        let differs = change.preview.as_ref().is_some_and(|preview| {
            serde_json::to_value(&action).is_ok_and(|applied| applied != *preview)
        });
        // What it wrote, for the task to read back at once: the workspace takes it in its own time.
        let wrote = self.wrote_since(&before);
        serde_json::json!({
            "ok": {"value": output.value, "resources": output.resources},
            "differs": differs,
            "change": wrote,
        })
    }

    /// What the document wrote since it held `before`, as the change a caller reads back.
    fn wrote_since(&self, before: &tilolang_tilo::chunk::Stamps) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(self.record.changes_since(before))
    }
}
