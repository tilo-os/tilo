//! Field edits offered by an installed app on resources in the workspace store.

use tilo_action_policy::{ActionKind, TreePath};
use tilo_actions::remote::Caller;
use tilo_agent_session::AgentSessions;
use tilo_agent_session::workspace_actions::PackagedCall;
use tilo_resource::path::{Edit, EditOp, Segment};
use tilo_resource_store::{Cell, Proposal, Resource, Schema, Stamp, Value};
use tilo_server_protocol::WorkspaceId;

use super::super::MemberDevice;
use super::super::packaged_model::{keep_surface, package_surface};
use super::Workspaces;

impl Workspaces {
    /// Apply a packaged field edit claimed by the action service for a live
    /// roster turn. The installed surface and current workspace decide the
    /// write, which is signed by the turn's joined device.
    ///
    /// # Errors
    /// The action, target, field or current roster session is not admitted,
    /// or the workspace could not commit the change.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked through commit"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "the invocation binding, current authority and admitted edit are checked together"
    )]
    pub async fn apply_roster_packaged_action(
        &self,
        workspace: WorkspaceId,
        sessions: &AgentSessions,
        caller: &Caller,
        mut call: PackagedCall<'_>,
    ) -> Result<serde_json::Value, String> {
        let denied = || "the roster resource action is not permitted".to_owned();
        if !caller.execution_is_live()
            || caller.turn().is_none()
            || call.arguments.len() != 1
            || call.action != tilo_resource::FIELD_EDIT
            || collab_types::store_document_readers(call.document)
                != Some(collab_types::EVERY_MEMBER)
        {
            return Err(denied());
        }
        let (session, delegation) = sessions.live_roster(workspace, caller).ok_or_else(denied)?;
        let identity = session.engine().identity();
        let device = MemberDevice {
            cert: identity.device_cert.clone(),
            signer: identity.signer.clone(),
        };
        let app = call.app;
        let kind = call.kind;
        let document = call.document;
        let surface_bytes =
            package_surface(call.package, app).map_err(|error| error.to_string())?;
        let (file, canonical) = collab_types::surface::SurfaceFile::kept(&surface_bytes)?;
        let surface = file.resource(Some(kind)).ok_or_else(denied)?;
        let verb = surface
            .verbs
            .iter()
            .find(|verb| verb.id == call.action)
            .ok_or_else(denied)?;
        if surface.person || verb.kind != ActionKind::Undoable || verb.destructive {
            return Err(denied());
        }
        let edit = Edit::<()>::from_json(
            call.arguments
                .remove(tilo_resource::EDIT_PARAM)
                .ok_or_else(denied)?,
        )?;
        let ([Segment::Item(id), Segment::Field(field)], EditOp::Set { value }) =
            (edit.path(), edit.op())
        else {
            return Err(denied());
        };
        let id = id.as_str();
        let schema = Schema::from_surface(surface);
        let Some(indexed_field) = schema
            .get(kind)
            .and_then(|ty| ty.fields.iter().find(|entry| entry.name == *field))
        else {
            return Err(denied());
        };
        if indexed_field.kind != tilo_resource_store::FieldKind::Scalar
            || !schema
                .verb(tilo_resource_store::FIELD_EDIT)
                .and_then(|editing| editing.writes.get(kind))
                .is_some_and(|writes| writes.fields.contains(field))
            || verb.field.as_ref() != Some(field)
        {
            return Err(denied());
        }
        let value = resource_value(value).ok_or_else(denied)?;
        let state = self
            .workspace(&workspace.to_string())
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let policy = self
            .roster_policy_in(
                &governed,
                caller,
                (&device.cert, &delegation),
                Some((app, document)),
            )
            .await
            .ok_or_else(denied)?;
        let agent = caller.acting().agent_id().ok_or_else(denied)?;
        let access = policy.agent_member_access(caller).ok_or_else(denied)?;
        let node = TreePath::workspace(governed.workspace())
            .app(app)
            .kind(kind)
            .resource(id);
        let now = tilo_clock::wall();
        if !access.allows(&node, ActionKind::Read, now)
            || !access.allows(
                &node.field(field).action(call.action),
                ActionKind::Undoable,
                now,
            )
            || !governed
                .peer()
                .governance()
                .app_version(app)
                .is_some_and(|installed| {
                    installed.release.file.as_deref()
                        == Some(blake3::hash(call.package).to_hex().as_str())
                        && installed.release.surface
                            == blake3::hash(canonical.as_bytes()).to_hex().as_str()
                })
        {
            return Err(denied());
        }
        keep_surface(&self.inner.host.surfaces, app, &surface_bytes);
        let stamp = Stamp {
            verb: call.action.to_owned(),
            invoker: Stamp::by(&caller.acting().by),
            invocation: call.invocation().0.to_string(),
            membership: governed
                .peer()
                .governance()
                .heads()
                .into_iter()
                .map(hex::encode)
                .collect(),
            ..Stamp::default()
        };
        let store = governed.store_with_surfaces(&self.inner.host.surfaces);
        let held = store
            .index()
            .get(kind, id)
            .filter(|held| held.doc == document)
            .ok_or_else(denied)?;
        let mut resource = Resource {
            readers: collab_types::EVERY_MEMBER.to_owned(),
            parent: held.parent.clone(),
            order: held.order.clone(),
            fields: held.fields.clone(),
        };
        resource.fields.insert(field.clone(), Cell::One(value));
        let admit = |proposal: &Proposal<'_>| {
            proposal.stamp == Some(&stamp)
                && proposal.touched.readers.is_none()
                && proposal.touched.resources.len() == 1
                && proposal.touched.resources.get(id).is_some_and(|touch| {
                    touch.kind.as_deref() == Some(kind)
                        && !touch.made
                        && !touch.removed
                        && !touch.placed
                        && !touch.content
                        && touch.fields.len() == 1
                        && touch.fields.contains(field)
                })
        };
        if let Some(write) = store.draft_field_update(
            (kind, id),
            &resource,
            &stamp,
            device.device(),
            Some(&schema),
            &admit,
        )? {
            self.commit_resource_update(&mut governed, &device, app, &write, |governed| {
                caller.execution_is_live()
                    && sessions
                        .live_roster(workspace, caller)
                        .is_some_and(|(_, live)| live == delegation)
                    && self
                        .approved_agent(
                            &governed.peer(),
                            workspace,
                            agent,
                            &device.cert.user_public_key,
                        )
                        .is_some_and(|(cert, current)| cert == device.cert && current == delegation)
            })
            .await?;
        }
        Ok(serde_json::Value::Null)
    }
}

fn resource_value(value: &serde_json::Value) -> Option<Value> {
    use serde_json::Value as Json;
    Some(match value {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Bool(*value),
        Json::String(value) => Value::Text(value.clone()),
        Json::Number(value) => {
            if let Some(value) = value.as_i64() {
                Value::Int(value)
            } else if value.is_f64() {
                Value::Float(value.as_f64()?)
            } else {
                return None;
            }
        }
        Json::Array(_) | Json::Object(_) => return None,
    })
}
