//! Resource commands from the hosted roster service. The broker supplies the
//! turn's caller and signing session in process.

use collab_types::{DocumentMutation, app_surface};
use tilo_action_policy::{ActionKind, TreePath, roles};
use tilo_actions::remote::Caller;
use tilo_agent_session::AgentSessions;
use tilo_peer_state::TransportOrigin;
use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Proposal, Schema, Stamp};

use super::super::sealed_documents::DocumentKeys;
use super::{Workspaces, signed_resource_write};

impl Workspaces {
    /// Carry an existing resource's field update from a hosted roster turn
    /// through the workspace's signed document admission and resource store.
    ///
    /// # Errors
    /// The roster signing session is unavailable, or the route, verb or
    /// fields are outside the caller's current authority.
    pub async fn hosted_resource_command(
        &self,
        sessions: &AgentSessions,
        caller: &Caller,
        (workspace, app): (&str, &str),
        command: Command,
    ) -> Result<Reply, String> {
        let denied = || "the hosted resource write is not permitted".to_owned();
        let workspace_id =
            tilo_server_protocol::WorkspaceId::parse(workspace).ok_or_else(denied)?;
        let agent = caller.acting().agent_id().ok_or_else(denied)?;
        let person = caller.acting_for_person().ok_or_else(denied)?;
        let (session, delegation) = sessions
            .live_roster(workspace_id, caller)
            .ok_or_else(denied)?;
        let identity = session.engine().identity();
        let device = identity.device_cert.device_public_key;
        let Command::Put {
            kind,
            id,
            resource,
            stamp,
        } = command
        else {
            return Err("a hosted resource command updates fields of an existing resource".into());
        };
        if (caller.execution_id().is_some() && !caller.execution_is_live())
            || resource.readers != collab_types::EVERY_MEMBER
            || !delegation.permits_app(app)
            || stamp.invocation.is_empty()
            || stamp.undoes.is_some()
        {
            return Err(denied());
        }
        let file = app_surface::surface_of(app).ok_or_else(denied)?;
        let surface = file.resource(Some(&kind)).ok_or_else(denied)?;
        let verb = surface
            .verbs
            .iter()
            .find(|verb| {
                verb.id == stamp.verb && verb.kind == ActionKind::Undoable && !verb.destructive
            })
            .ok_or_else(denied)?;

        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let doc = governed
            .resources
            .store_mut()
            .index()
            .get(&kind, &id)
            .map(|held| held.doc.clone())
            .ok_or_else(denied)?;
        let keys = governed.route_keys().await.ok_or_else(denied)??;
        let policy = self
            .roster_policy_in(
                &governed,
                caller,
                (&identity.device_cert, &delegation),
                Some((app, &doc)),
            )
            .await
            .ok_or_else(denied)?;
        let mut acting = caller.acting().clone();
        acting.foreign_device = policy.access.acting.foreign_device;
        let access = policy
            .access
            .as_acting(acting)
            .ok_or_else(denied)?
            .granted(roles::agent_bound(agent, person, &policy.agent_bound));
        let node = TreePath::workspace(workspace)
            .app(app)
            .kind(&kind)
            .resource(&id);
        let action = match &verb.field {
            Some(field) => node.clone().field(field),
            None => node.clone(),
        }
        .action(&verb.id);
        let now = tilo_clock::wall();
        if !access.allows(&node, ActionKind::Read, now)
            || !access.allows(&action, ActionKind::Undoable, now)
        {
            return Err(denied());
        }
        let admit = |proposal: &Proposal<'_>| {
            proposal.touched.readers.is_none()
                && proposal.touched.resources.len() == 1
                && proposal.touched.resources.get(&id).is_some_and(|touch| {
                    touch.kind.as_deref() == Some(kind.as_str())
                        && !touch.made
                        && !touch.removed
                        && !touch.placed
                        && !touch.content
                        && touch.fields.iter().all(|field| {
                            access.allows(
                                &node.clone().field(field).action("field-edit"),
                                ActionKind::Undoable,
                                now,
                            )
                        })
                })
        };
        let stamp = Stamp {
            invoker: agent.to_owned(),
            membership: governed
                .peer()
                .governance()
                .heads()
                .into_iter()
                .map(hex::encode)
                .collect(),
            ..stamp
        };
        let schema = Schema::from_surface(surface);
        if surface.person || surface.own || schema.get(&kind).is_none() {
            return Err(denied());
        }
        let Some(draft) = governed.resources.store_mut().draft_field_update(
            (&kind, &id),
            &resource,
            &stamp,
            device,
            &schema,
            &admit,
        )?
        else {
            return Ok(Reply::Written);
        };
        let write = draft.written();
        let hash = write.change.hash();
        let mutation = DocumentMutation::Incremental(write.change.raw_bytes().to_vec());
        let mut candidate = governed.peer().clone();
        let envelope = signed_resource_write(
            &candidate,
            (workspace, app, &doc),
            mutation,
            &keys,
            identity.signer.as_ref(),
            now,
        )?;
        let message = neighbour_wire::NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&device),
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document_id: doc.clone(),
            envelope,
            own: candidate.own_write(app, &doc, &device),
            readers: None,
        };
        candidate
            .receive_from(
                TransportOrigin::RetainedServerDelta(device),
                message.clone(),
                now,
            )
            .map_err(|error| error.to_string())?;
        let document_keys = DocumentKeys::of(
            keys,
            workspace,
            &write.readers,
            &collab_types::app_document_id(app, &doc),
        );
        let opened = candidate
            .publication()
            .opened_document(app, &doc, &mut document_keys.open())?
            .ok_or_else(denied)?;
        if !automerge::Automerge::load(&opened.document)
            .map_err(|error| error.to_string())?
            .has_change(&hash)
        {
            return Err(denied());
        }
        // No change of this draft reaches either live document until both
        // admissions have accepted it and the signed envelope is durable.
        if (caller.execution_id().is_some() && !caller.execution_is_live())
            || sessions.live_roster(workspace_id, caller).is_none()
            || self
                .approved_agent(
                    &governed.peer(),
                    workspace_id,
                    agent,
                    &identity.device_cert.user_public_key,
                )
                .as_ref()
                != Some(&(identity.device_cert.clone(), delegation))
        {
            return Err(denied());
        }
        self.retain_document(workspace_id, device, &message).await?;
        *governed.peer.lock() = candidate;
        self.landed(&governed, &message);
        self.keep_soon(&mut governed);
        let before = governed.resources.heads(&doc);
        governed
            .resources
            .store_mut()
            .apply_field_update(draft)
            .map_err(|error| error.to_string())?;
        governed.resources.kept(&doc, &before)?;
        self.tell_followers(&governed, (app.to_owned(), doc), &message)
            .await;
        self.carry_to_links(workspace, device, &message);
        Ok(Reply::Written)
    }
}
