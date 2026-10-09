//! A participant without a window: an agent, a service or a script that
//! joins a record, holds a replica of it, and edits it as a person does.
//!
//! A [`Participant`] knows the record only by its [`ResourceKind`], so it
//! works with any registered resource type and links none of its code. What
//! it may do is exactly what the resource publishes
//! ([`tilo_resource::publish`]) and the participant's permissions allow:
//! every edit goes through the same door a control, the palette or a script
//! uses — resolved against the tree as it stands now, checked against the
//! action's own permission and confirmation, applied by the replayable
//! executor under the participant's actor, attributed to it
//! ([`ActionCx::author`](tilo_resource::ActionCx::author)) and recorded in
//! history with the task it belongs to. A typed edit with no named
//! declaration travels as the resource's generic `apply`, several as one
//! `group`: one change, one version, one undo step.
//!
//! Sync is the caller's: [`Participant::merge_snapshot`] and
//! [`Participant::merge_changes`] take what peers sent, and every edit
//! answers with the changes to send them.

use tilo_actions::door::{self, ActionAddress, ActionDescriptor, DoorError, InvocationProvenance};
use tilo_actions::{ActionContext, ActionInvoker, ActionRegistry, LocalTransport, OperationKind};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Doc, GROUP, GROUP_PARAM, ModelError, Place, Resource, ResourceKind};
use ui_core::automerge;
use ui_core::permissions::PermissionContext;

/// One record held and edited without a window. See the
/// [module docs](self).
pub struct Participant {
    kind: ResourceKind,
    doc: Doc,
    /// Who the participant is: the author its edits are attributed to.
    me: String,
    /// The Automerge actor its changes are written under.
    actor: Vec<u8>,
    permissions: PermissionContext,
    /// The task or run the next edits belong to.
    context: Option<ActionContext>,
    registry: ActionRegistry,
    /// Where its last invocation left it.
    place: Option<Place>,
}

impl Participant {
    /// A participant `me` holding an empty replica of a record of `kind`,
    /// which the first snapshot a peer sends fills
    /// ([`Self::merge_snapshot`]). Its changes are written under the actor
    /// `me` names until [`Self::writing_as`] says otherwise.
    #[must_use]
    pub fn new(kind: ResourceKind, me: &str, permissions: PermissionContext) -> Self {
        let mut registry = ActionRegistry::new();
        kind.register(&mut registry);
        Self {
            kind,
            doc: Doc::new(),
            me: me.to_owned(),
            actor: me.as_bytes().to_vec(),
            permissions,
            context: None,
            registry,
            place: None,
        }
    }

    /// The same participant seeded from a peer's whole document.
    ///
    /// # Errors
    /// Returns why `bytes` are not a document holding this kind of resource.
    pub fn from_snapshot(
        kind: ResourceKind,
        bytes: &[u8],
        me: &str,
        permissions: PermissionContext,
    ) -> Result<Self, ModelError> {
        let doc = Doc::load(bytes)?;
        kind.read(&doc)?;
        let mut participant = Self::new(kind, me, permissions);
        participant.doc = doc;
        participant.drain();
        Ok(participant)
    }

    /// Write changes under `actor`: one Automerge actor per replica, when
    /// one participant holds several records.
    #[must_use]
    pub fn writing_as(mut self, actor: &[u8]) -> Self {
        self.actor = actor.to_vec();
        self
    }

    /// The kind of resource held.
    pub fn kind(&self) -> &ResourceKind {
        &self.kind
    }

    /// Who the participant is.
    pub fn me(&self) -> &str {
        &self.me
    }

    /// The Automerge actor its changes are written under.
    pub fn actor(&self) -> &[u8] {
        &self.actor
    }

    /// The participant's permissions as they stand.
    pub fn permissions(&self) -> &PermissionContext {
        &self.permissions
    }

    /// The replica, to read.
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The resource's state, as the type the caller expects.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the replica holds no such resource yet or
    /// its state is not an `S`.
    pub fn state<S: 'static>(&self) -> Result<S, ModelError> {
        self.kind.read_as(&self.doc)
    }

    /// The task or run the next edits belong to, recorded with each in
    /// history; `None` once it is over, so a later edit does not inherit it.
    pub fn set_context(&mut self, context: Option<ActionContext>) {
        self.context = context;
    }

    /// The participant's permissions changed: a role granted or taken away.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        self.permissions = permissions;
    }

    /// The tree the resource publishes now, as the participant may reach
    /// it: the operations it may perform, under the addresses the app's own
    /// window publishes. Empty while the replica holds no such resource.
    pub fn resources(&self) -> Vec<tilo_actions::Resource> {
        let Ok(published) = self.kind.publish(self.kind.kind(), &self.doc) else {
            return Vec::new();
        };
        self.permissions
            .permitted_resource(self.app_id(), published)
            .into_iter()
            .collect()
    }

    /// Every document operation the participant may perform now, with its
    /// typed parameters. View and effect operations are the app's own to
    /// run, so they are not here.
    pub fn operations(&self) -> Vec<ActionDescriptor> {
        door::survey(&self.resources())
            .into_iter()
            .filter(|descriptor| descriptor.kind == OperationKind::Document)
            .collect()
    }

    /// Perform the operation `address` names with typed `arguments`, as the
    /// participant. Returns the changes to send to peers, `None` when the
    /// operation changed nothing.
    ///
    /// # Errors
    /// The door's refusal: [`DoorError::Unknown`] for an address the tree
    /// no longer carries or the participant may not reach,
    /// [`DoorError::ConfirmationRequired`] for an operation that needs a
    /// person's confirmation, bad arguments, or the action's own failure.
    pub fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        self.invoke_as(address, arguments, false)
    }

    /// [`Self::invoke`] after the person it acts for confirmed it.
    ///
    /// # Errors
    /// As [`Self::invoke`], except that no confirmation is required.
    pub fn invoke_confirmed(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        self.invoke_as(address, arguments, true)
    }

    /// Apply one typed action of the resource, given as JSON, through its
    /// generic `apply`: permission-checked, attributed and named in history
    /// after what it does.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn apply(&mut self, action: serde_json::Value) -> Result<Option<Vec<u8>>, DoorError> {
        let mut arguments = serde_json::Map::new();
        arguments.insert(tilo_resource::APPLY_PARAM.to_owned(), action);
        self.invoke(&self.address(tilo_resource::APPLY), &arguments)
    }

    /// Apply several typed actions as one: one change, one version in
    /// history, one undo step; if any refuses, nothing is applied.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn apply_all(
        &mut self,
        actions: impl IntoIterator<Item = serde_json::Value>,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        let members = actions
            .into_iter()
            .map(|action| {
                serde_json::json!({
                    "action": tilo_resource::APPLY,
                    "args": { tilo_resource::APPLY_PARAM: action },
                })
            })
            .collect();
        let mut arguments = serde_json::Map::new();
        arguments.insert(GROUP_PARAM.to_owned(), serde_json::Value::Array(members));
        self.invoke(&self.address(GROUP), &arguments)
    }

    /// [`Self::apply`] for a resource type the caller links.
    ///
    /// # Errors
    /// As [`Self::invoke`]; [`DoorError::InvalidArguments`] when `R` is not
    /// the kind held.
    pub fn apply_typed<R: Resource>(
        &mut self,
        action: &R::Action,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        self.apply_all_typed::<R>(std::slice::from_ref(action))
    }

    /// [`Self::apply_all`] for a resource type the caller links. One action
    /// is applied alone, not as a group of one.
    ///
    /// # Errors
    /// As [`Self::apply_typed`].
    pub fn apply_all_typed<R: Resource>(
        &mut self,
        actions: &[R::Action],
    ) -> Result<Option<Vec<u8>>, DoorError> {
        if R::KIND != self.kind.kind() {
            return Err(DoorError::InvalidArguments(format!(
                "a {} action does not apply to a {}",
                R::LABEL,
                self.kind.label()
            )));
        }
        let mut values = actions
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DoorError::InvalidArguments(error.to_string()))?;
        match values.len() {
            1 => self.apply(values.remove(0)),
            _ => self.apply_all(values),
        }
    }

    fn invoke_as(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        let resources = self.resources();
        // The tree offers only what the participant may do; the address may
        // have come from elsewhere.
        if let Some((resource, spec)) = door::locate(&resources, address)
            && !self.permissions.permits_invocation(
                self.app_id(),
                resource,
                spec,
                Some(arguments),
                None,
            )
        {
            return Err(DoorError::Unavailable {
                code: "permission_denied".into(),
                reason: format!("you may not {} here", spec.label.to_lowercase()),
            });
        }
        let ambient = self.ambient();
        let provenance = InvocationProvenance {
            actor: &self.actor,
            ambient: Some(&ambient),
        };
        let mut transport = LocalTransport::new();
        let invoke = if confirmed {
            door::invoke_json_confirmed
        } else {
            door::invoke_json
        };
        invoke(
            &mut self.doc,
            &resources,
            address,
            arguments,
            provenance,
            &self.registry,
            &mut transport,
        )?;
        self.place = transport
            .replicated
            .last()
            .and_then(|envelope| self.kind.lands(&self.doc, &envelope.action))
            .or_else(|| {
                (address.resource != self.kind.kind())
                    .then(|| Place::Item(address.resource.clone()))
            });
        let changes = self.doc.save_incremental();
        Ok((!changes.is_empty()).then_some(changes))
    }

    /// Where the participant's last invocation left it: where the action
    /// lands ([`Resource::lands`]), else the item it was taken on, else
    /// nowhere in particular. What its presence shows collaborators.
    pub fn place(&self) -> Option<&Place> {
        self.place.as_ref()
    }

    /// History names the app, the participant and the task each edit
    /// belongs to.
    fn ambient(&self) -> ActionContext {
        let mut named = ActionContext::default().with_invoker(ActionInvoker::actor(&self.me));
        if let (Some(id), Some(title)) = (self.kind.app_id(), self.kind.app_title()) {
            named = named.with_app(id, title);
        }
        match &self.context {
            Some(context) => context.clone().enriched_by(&named),
            None => named,
        }
    }

    /// The app permissions name, or the kind when no app registered it.
    fn app_id(&self) -> &'static str {
        self.kind.app_id().unwrap_or_else(|| self.kind.kind())
    }

    fn address(&self, action: &str) -> ActionAddress {
        ActionAddress::new(self.kind.kind(), action)
    }

    /// Merge a peer's whole document. Never replaces: both sides keep what
    /// the other had not seen. Returns whether it was taken; bytes that are
    /// not a document of this kind, or will not merge with this one, are
    /// another record's and are left alone.
    pub fn merge_snapshot(&mut self, bytes: &[u8]) -> bool {
        let Ok(mut incoming) = Doc::load(bytes) else {
            return false;
        };
        if !self.kind.holds(&incoming) || self.doc.merge_incoming(&mut incoming).is_err() {
            return false;
        }
        self.drain();
        true
    }

    /// Apply a peer's incremental changes. Returns whether the replica
    /// moved. Changes whose ancestors this replica has not got belong to
    /// another record (or arrived ahead of what they build on) and are left
    /// alone rather than queued, so the replica never reads as complete
    /// while it waits for something that may never come.
    pub fn merge_changes(&mut self, bytes: &[u8]) -> bool {
        let mut incoming = Doc::new();
        if incoming.load_incoming(bytes).is_err() {
            return false;
        }
        if !incoming
            .get_missing_deps(&[])
            .iter()
            .all(|hash| self.doc.get_change_by_hash(hash).is_some())
        {
            return false;
        }
        let before = self.doc.get_heads();
        if self.doc.load_incoming(bytes).is_err() {
            return false;
        }
        self.drain();
        self.doc.get_heads() != before
    }

    /// How many ancestor changes the replica is waiting for.
    pub fn missing_changes(&mut self) -> usize {
        self.doc.get_missing_deps(&[]).len()
    }

    /// The whole document, for a peer that just arrived.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Record one instruction that changed several records as one run in
    /// this record's history ([`ui_core::record_run_with_details`]), by the
    /// participant, so a person can review and undo it as one thing.
    /// Returns the changes to send to peers; `None` when there were no
    /// steps to record.
    pub fn record_run(
        &mut self,
        label: &str,
        issued_by: Option<&str>,
        decision: Option<&str>,
        steps: &[ui_core::RunStep],
    ) -> Option<Vec<u8>> {
        let actor = automerge::ActorId::from(self.actor.as_slice());
        let by = actor.to_hex_string();
        let previous = self.doc.get_actor().clone();
        self.doc.set_actor(actor);
        let recorded = ui_core::record_run_with_details(
            &mut self.doc,
            label,
            Some(&by),
            issued_by,
            Some(label),
            decision,
            steps,
        );
        if recorded.is_some() {
            tilo_resource::history::commit_stamped(&mut self.doc);
        }
        self.doc.set_actor(previous);
        recorded?;
        let changes = self.doc.save_incremental();
        (!changes.is_empty()).then_some(changes)
    }

    /// The replica's current heads.
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// The changes the replica holds that `heads` do not.
    pub fn changes_since(&mut self, heads: &[automerge::ChangeHash]) -> Vec<automerge::Change> {
        self.doc.get_changes(heads)
    }

    /// The record's version history, each version attributed and named.
    pub fn versions(&mut self) -> Vec<tilo_resource::history::Version> {
        tilo_resource::history::versions(&mut self.doc)
    }

    /// What was merged from peers is not this participant's to send back.
    fn drain(&mut self) {
        let _ = self.doc.save_incremental();
    }
}
