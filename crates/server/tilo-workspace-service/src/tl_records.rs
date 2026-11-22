//! A tilolang app's record of a workspace, held on a device beside the workspace's documents:
//! read through the app's own package, changed through its own actions as a given member, and
//! the change written into the workspace as that member's write, signed by this device.
//!
//! This is what a device's own roles use when they act on tilolang resources with no window of
//! the app open: the agent engine answering an ask ([`crate::asks`]), a bridge posting into a
//! channel, a moderator reading reports. Nothing here is per app.
//!
//! - **Packages** ([`Packages`]): the packages a record is read through, compiled once and kept,
//!   in the order they load (what an app uses first, the app last).
//! - **The record** ([`RecordBytes`]): an app's document of the workspace and the chunks above
//!   it, as the workspace holds them ([`crate::workspaces::Workspaces::tl_record`]).
//! - **Opened** ([`Opened`]): a tilolang host over those bytes on the calling thread. It reads any
//!   resource ([`Opened::host`]), and runs any action of a loaded package as any member
//!   ([`Opened::invoke`]): the action's own code plans it, its edits are applied where the
//!   record's manifest places them, and the host takes them in. What a mention in written text
//!   asks is made as the host's viewer, as on any device ([`tilolang_tilo::host::Host::ask_from_mentions`]).
//! - **Written** ([`Opened::written`]): what the actions wrote, one change per document, which
//!   [`crate::workspaces::Workspaces::write_tl`] writes as this device's own: signed, sealed under the
//!   document's group where the workspace seals, and committed through the one commit path.
//!
//! [`crate::workspaces::Workspaces::with_tl_record`] does all three for one call. A host is
//! bound to its thread, so the call runs on a blocking thread of its own and hands back only what
//! the caller's closure returns.
//!
//! Who may do what is decided where every member's write is decided: the workspace admits the
//! written change against the app's surface and the member's grants. The caller names the member
//! it acts as; code on this device that calls here is the device's own.

use std::collections::BTreeMap;
use std::sync::Arc;

use automerge::ChangeHash;
use tilo_actions::{ActionContext, ActionInvoker, InvocationId};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::chunk::{self, ChunkNo, Chunks, FIRST};
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, InvokeOutcome, ResourceRef, Value};

/// The packages a record is read and changed through, compiled once: those the app uses first,
/// the app itself last.
#[derive(Clone)]
pub struct Packages(Arc<Vec<tilolang_tilo::tilolang::Compiled>>);

impl Packages {
    /// Compile `sources` (each a package's `app.tl` and its `app.ids`), each against those
    /// before it.
    ///
    /// # Errors
    /// A source does not compile: its diagnostics.
    pub fn compile(sources: &[(&str, &str)]) -> Result<Self, String> {
        let mut compiled: Vec<tilolang_tilo::tilolang::Compiled> = Vec::new();
        for (source, ids) in sources {
            let lock = tilolang_tilo::tilolang::IdsLock::parse(ids)
                .map_err(|error| error.to_string())?;
            let imports: Vec<_> = compiled.iter().map(|done| done.meta.clone()).collect();
            let package = tilolang_tilo::tilolang::compile_package(source, &lock, &imports)
                .map_err(|diagnostics| {
                    diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.render("app.tl"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })?;
            compiled.push(package);
        }
        Ok(Self(Arc::new(compiled)))
    }

    /// The app the record is of: the last package's.
    #[must_use]
    pub fn app(&self) -> &str {
        self.0.last().map_or("", |package| package.meta.app.as_str())
    }
}

/// An app's record of a workspace as bytes: its document (the app's primary, `""`) and the
/// chunks above it, by document id. A document the workspace does not hold yet is absent.
#[derive(Clone, Debug, Default)]
pub struct RecordBytes {
    pub documents: BTreeMap<String, Vec<u8>>,
}

/// The chunk the document `document` of a record is, or `None` for no document of the record.
#[must_use]
pub fn chunk_of_document(document: &str) -> Option<ChunkNo> {
    if document.is_empty() {
        return Some(FIRST);
    }
    let number = document.strip_prefix("derived:chunk.")?.strip_suffix('@')?;
    number.parse().ok()
}

/// The document chunk `chunk` of a record is kept in.
#[must_use]
pub fn document_of_chunk(chunk: ChunkNo) -> String {
    if chunk == FIRST {
        String::new()
    } else {
        chunk::document_id("", chunk)
    }
}

/// One document's change, as [`Opened::written`] answers it: the document, its changes since it
/// was read, and whether this write founds it (then the changes are the document whole).
#[derive(Clone, Debug)]
pub struct Written {
    pub document: String,
    pub changes: Vec<u8>,
    pub founding: bool,
}

/// An app's record opened on this thread: a host over it with its packages loaded.
pub struct Opened {
    host: Host,
    app: String,
    chunks: Chunks,
    /// The heads each document stood at when it was read; a chunk missing here was not held.
    read_at: BTreeMap<ChunkNo, Vec<ChangeHash>>,
}

impl Opened {
    /// Open `record` through `packages` for `viewer`: the member whose device this is, as whom
    /// what its written text asks is asked.
    ///
    /// # Errors
    /// A document does not load, or a package does not.
    pub fn open(record: &RecordBytes, packages: &Packages, viewer: Actor) -> Result<Self, String> {
        let mut chunks = Chunks::default();
        let mut read_at = BTreeMap::new();
        for (document, bytes) in &record.documents {
            let Some(chunk) = chunk_of_document(document) else {
                continue;
            };
            let mut doc = tilo_resource::Doc::load(bytes).map_err(|error| error.to_string())?;
            read_at.insert(chunk, doc.get_heads());
            chunks.insert(chunk, doc);
        }
        chunks.reread_placement();
        let first = chunks.first().cloned().unwrap_or_default();
        let host = Host::new(&first, viewer, || None).map_err(|error| error.to_string())?;
        for package in packages.0.iter() {
            host.load_compiled(package)
                .map_err(|error| error.to_string())?;
        }
        host.sync(&chunks);
        let mut opened = Self {
            host,
            app: packages.app().to_owned(),
            chunks,
            read_at,
        };
        // What the host did of its own as it took the record in (a mention's ask) is the
        // viewer's, written as any change made here.
        opened.commit()?;
        Ok(opened)
    }

    /// The host the record is read through: [`Host::runtime`] reads any field of any resource,
    /// [`Host::store`] its history and links.
    #[must_use]
    pub const fn host(&self) -> &Host {
        &self.host
    }

    /// The app the record is of.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// Run `app`'s action `action` with `args` as `who`, made from `from` when given (what it
    /// creates is made from that resource: `created(r)?.from`). Answers what the action returned.
    ///
    /// # Errors
    /// The package has no such action, the action was refused or failed, or what it wrote does
    /// not apply.
    pub fn invoke(
        &mut self,
        who: &Actor,
        (app, action): (&str, &str),
        args: Vec<Value>,
        from: Option<&ResourceRef>,
    ) -> Result<Value, String> {
        let package = self
            .host
            .package(app)
            .ok_or_else(|| format!("{app} is not loaded here"))?;
        let id = package
            .meta
            .actions
            .iter()
            .find(|declared| declared.name == action)
            .map(|declared| declared.id.clone())
            .ok_or_else(|| format!("{app} has no action {action}"))?;
        let runtime = self.host.runtime();
        let outcome = match from {
            Some(from) => runtime.invoke_action_from(who, (id.as_str(), args), from),
            None => runtime.invoke_action(who, (id.as_str(), args)),
        }
        .map_err(|error| error.to_string())?;
        let value = match outcome {
            InvokeOutcome::Done(value) => value,
            InvokeOutcome::Denied => return Err(format!("{app}#{action} was refused")),
            InvokeOutcome::Suggested(_) => {
                return Err(format!("{app}#{action} waits for an answer"));
            }
        };
        self.commit()?;
        Ok(value)
    }

    /// Apply every action the host committed since the last call to the record, and take the
    /// record back into the host; again while taking it in commits more (a mention's ask).
    fn commit(&mut self) -> Result<(), String> {
        loop {
            let committed = self.host.take_committed();
            if committed.is_empty() {
                return Ok(());
            }
            for entry in committed {
                self.apply(&entry.action)?;
            }
            self.host.sync(&self.chunks);
        }
    }

    /// Apply `action` where its edits belong: each chunk it writes holds its part as one change,
    /// and every part names the one invocation the action ran in.
    fn apply(&mut self, action: &DynAction) -> Result<(), String> {
        let actor = action.invoker();
        let invoker = ActionInvoker::admitted(&actor.clone().into());
        let invocation = action
            .authorship
            .invocation
            .as_deref()
            .map_or_else(InvocationId::new, tilolang_tilo::action::invocation_id);
        let mut first = false;
        for (chunk, part) in self.chunks.parts_of(action) {
            let encoded = part.encode().map_err(|error| error.to_string())?;
            let context = ActionContext::for_resource(
                part.resources().first().copied().unwrap_or("workspace"),
                &part.kind,
                &part.kind,
            )
            .with_app(&self.app, "tilolang")
            .with_source_invocation(Some(invocation))
            .in_gesture(action.authorship.gesture)
            .with_invoker(invoker.clone());
            let doc = self.chunks.open(chunk);
            self.host
                .with_schemas(|| {
                    tilo_actions::apply_local_with_context(
                        doc,
                        &part,
                        encoded,
                        Some(&actor.actor_bytes()),
                        context,
                    )
                })
                .map_err(|error| error.to_string())?;
            doc.commit();
            first |= chunk == FIRST;
        }
        if first {
            self.chunks.reread_placement();
        }
        Ok(())
    }

    /// What the actions run here wrote, one change per document: the changes since the document
    /// was read, or the document whole where this founds it.
    #[must_use]
    pub fn written(&self) -> Vec<Written> {
        let mut written = Vec::new();
        for (chunk, doc) in self.chunks.all() {
            let document = document_of_chunk(chunk);
            let mut doc = doc.clone();
            match self.read_at.get(&chunk) {
                Some(heads) if doc.get_heads() == *heads => {}
                Some(heads) => {
                    written.push(Written {
                        document,
                        changes: doc.save_after(heads),
                        founding: false,
                    });
                }
                None => {
                    written.push(Written {
                        document,
                        changes: doc.save(),
                        founding: true,
                    });
                }
            }
        }
        written
    }
}

/// The member `id` names, as a tilolang actor.
#[must_use]
pub fn member(id: tilo_actions::MemberId) -> Actor {
    Actor::Member(id)
}

impl crate::workspaces::Workspaces {
    /// `app`'s record of `workspace`, as this device holds it: its document and every chunk
    /// above it this device reads.
    ///
    /// # Errors
    /// The workspace is not held here, or a document of it cannot be read.
    pub async fn tl_record(&self, workspace: &str, app: &str) -> Result<RecordBytes, String> {
        let routes = self
            .document_routes(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut record = RecordBytes::default();
        for (of, document) in routes {
            if of != app || chunk_of_document(&document).is_none() {
                continue;
            }
            let bytes = self
                .resource_document(workspace, app, &document)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(bytes) = bytes {
                record.documents.insert(document, bytes);
            }
        }
        Ok(record)
    }

    /// Write `written`, changes of `app`'s record of `workspace`, as this device's own: each
    /// document's change signed, sealed under its group where the workspace seals, and
    /// committed.
    ///
    /// # Errors
    /// The workspace refused a write, or its keys could not be had: what was written before it
    /// stands.
    pub async fn write_tl(
        &self,
        workspace: &str,
        app: &str,
        written: Vec<Written>,
    ) -> Result<(), String> {
        for write in written {
            let route = tilo_peer_state::document_frame::DocumentRoute {
                app_id: app.to_owned(),
                cluster_id: workspace.to_owned(),
                document: write.document.clone(),
            };
            let sealed = match self.own_route_keys(&route, write.founding).await {
                Some(Ok(keys)) => Some(keys),
                Some(Err(reason)) => return Err(reason),
                None => None,
            };
            self.own_write(
                workspace,
                (app, &write.document),
                None,
                &write.changes,
                write.founding,
                write.founding,
                sealed.as_ref(),
            )
            .await
            .map_err(|refusal| refusal.to_string())?;
        }
        Ok(())
    }

    /// Open `packages`' app's record of `workspace` for `viewer`, run `work` on it, and write what
    /// it changed as this device's own ([`Self::write_tl`]): what `work` answered. The record is
    /// read anew for each call, so `work` acts on what the workspace holds now.
    ///
    /// # Errors
    /// The record could not be read or opened, `work` failed (nothing it changed is written), or
    /// the write was refused.
    pub async fn with_tl_record<R: Send + 'static>(
        &self,
        workspace: &str,
        packages: &Packages,
        viewer: Actor,
        work: impl FnOnce(&mut Opened) -> Result<R, String> + Send + 'static,
    ) -> Result<R, String> {
        let app = packages.app().to_owned();
        let record = self.tl_record(workspace, &app).await?;
        let packages = packages.clone();
        let (answer, written) = tokio::task::spawn_blocking(move || {
            let mut opened = Opened::open(&record, &packages, viewer)?;
            let answer = work(&mut opened)?;
            Ok::<_, String>((answer, opened.written()))
        })
        .await
        .map_err(|error| error.to_string())??;
        self.write_tl(workspace, &app, written).await?;
        Ok(answer)
    }
}
