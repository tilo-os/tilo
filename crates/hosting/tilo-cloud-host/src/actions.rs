//! Bounded native application operations. No arbitrary effects or shell commands.
use super::*;
#[cfg(test)]
use collab_types::caps::Principal;
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
#[cfg(test)]
use tilo_actions::ActionRegistry;
mod media;
#[cfg(test)]
pub(crate) mod media_tests;
#[cfg(test)]
mod policy_time_tests;
/// Addressed application protocol, separate from workspace fan-out.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub version: u32,
    pub room: String,
    pub invocation_id: String,
    pub operation: NativeOperation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeOperation {
    /// Test fixture: insert into the canonical Writer body's first paragraph
    /// through the real app action and the owner's permission checks.
    #[cfg(test)]
    WriterInsert {
        text: String,
    },
    /// Issue one room-scoped invitation through the authenticated Owner path.
    CollaborationInvite {
        role: tilo_peer_state::join::MemberRole,
        recipient_user_public_key: Option<[u8; 32]>,
        invitee_name: Option<String>,
        expires_at_ns: u64,
    },
    /// Revoke a previously issued invitation before redemption.
    CollaborationCancelInvitation {
        invite_id: [u8; 32],
    },
    /// List the current Owner's hosted invitation records.
    CollaborationListInvitations,
    /// Change an admitted independent-root member's room role.
    CollaborationSetRole {
        user_public_key: [u8; 32],
        role: tilo_peer_state::join::MemberRole,
    },
    /// Remove an admitted independent-root member from live governed access.
    CollaborationRemove {
        user_public_key: [u8; 32],
    },
    /// Governed media mutations retain the existing certified caller and receipt scope.
    MediaCreate(tilo_action_broker::collaboration_admin::MediaCreateInput),
    MediaEnd(tilo_action_broker::collaboration_admin::MediaCallInput),
    MediaRestart(tilo_action_broker::collaboration_admin::MediaCallInput),
    MediaSetDirections(tilo_action_broker::collaboration_admin::MediaDirectionsChangeInput),
    MediaSetParticipantDenial(tilo_action_broker::collaboration_admin::MediaDenialInput),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub invocation_id: String,
    pub status: String,
    pub error: Option<String>,
    pub output: Option<serde_json::Value>,
}
impl Store {
    pub fn action(
        &mut self,
        source: [u8; 32],
        request: &ActionRequest,
        now: u64,
    ) -> Result<ActionReceipt> {
        self.ensure_reconciled()?;
        let now = self.policy_now_or(now)?;
        if request.version != 1
            || request.room != self.state.room
            || request.invocation_id.is_empty()
            || request.invocation_id.len() > 128
        {
            bail!("invalid action identity")
        }
        let (before, mut peer) = self.peer_transition_at(now)?;
        action_permissions(&peer, source, request, now)?;
        let scope = format!("{}:{}", hex::encode(source), request.invocation_id);
        let digest = blake3::hash(&serde_json::to_vec(request)?)
            .to_hex()
            .to_string();
        if let Some((prior, receipt)) = self.state.receipts.get(&scope) {
            if prior == &digest || (prior == "cancelled" && receipt.status == "cancelled") {
                return Ok(receipt.clone());
            }
            bail!("invocation substitution")
        }
        let mut state = self.state.clone();
        // Preparing a request can cross a membership/certificate boundary.
        // Derive the actor and its decision time together from the still-locked owner.
        let now = self.policy_now_or(now)?;
        let (role, _caps, _person, _actor) = action_permissions(&peer, source, request, now)?;
        if let Some(operation) = media::operation(&request.operation) {
            anyhow::ensure!(
                self.state.version == crate::delivery::VERSION && peer.admission_v5_selected(),
                "selected Cloud delivery media writer required"
            );
            let (mut peer, result) =
                tilo_workspace_host::media_governance::prepare_media_transition(
                    peer,
                    &self.key(),
                    &operation,
                    now_time(self.policy_now_or(now)?),
                )?;
            state.outbox.extend(durable_mutation_publications(
                &mut peer,
                &before,
                &self.key(),
                now_time(self.policy_now_or(now)?),
                self.next_generation()?,
            )?);
            state
                .bound
                .as_mut()
                .ok_or_else(|| anyhow!("workspace missing"))?
                .peer = peer
                .durable()
                .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
            let receipt = ActionReceipt {
                invocation_id: request.invocation_id.clone(),
                status: "completed".into(),
                error: None,
                output: Some(serde_json::to_value(result)?),
            };
            state.receipts.insert(scope, (digest, receipt.clone()));
            self.commit(state)?;
            return Ok(receipt);
        }
        if matches!(
            request.operation,
            NativeOperation::CollaborationInvite { .. }
                | NativeOperation::CollaborationCancelInvitation { .. }
                | NativeOperation::CollaborationListInvitations
                | NativeOperation::CollaborationSetRole { .. }
                | NativeOperation::CollaborationRemove { .. }
        ) {
            let caller = peer
                .certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == source)
                .ok_or_else(|| anyhow!("collaboration administrator certificate missing"))?;
            let operation_name = match request.operation {
                NativeOperation::CollaborationInvite { .. } => "invitation.issue",
                NativeOperation::CollaborationCancelInvitation { .. } => "invitation.cancel",
                NativeOperation::CollaborationListInvitations => "invitation.list",
                NativeOperation::CollaborationSetRole { .. } => "member.role.set",
                NativeOperation::CollaborationRemove { .. } => "member.remove",
                _ => "unknown",
            };
            if !matches!(role, collab_types::Role::Owner)
                || peer.authority().user_public_key != caller.user_public_key
            {
                audit(
                    "collaboration_admin_denied",
                    serde_json::json!({
                        "room": self.state.room,
                        "caller_device": hex::encode(source),
                        "caller_root": hex::encode(caller.user_public_key),
                        "invocation_id": request.invocation_id,
                        "operation": operation_name,
                        "result": "denied_not_structural_owner",
                    }),
                );
                bail!("collaboration administration requires current Owner authority")
            }
            let output = match &request.operation {
                NativeOperation::CollaborationInvite {
                    role,
                    recipient_user_public_key,
                    invitee_name,
                    expires_at_ns,
                } => {
                    if *expires_at_ns <= now
                        || *expires_at_ns > now.saturating_add(30u64 * 24 * 60 * 60 * 1_000_000_000)
                        || invitee_name.as_ref().is_some_and(|name| {
                            name.trim().is_empty()
                                || name.len() > 128
                                || name.chars().any(char::is_control)
                        })
                    {
                        bail!("invalid collaboration invitation")
                    }
                    let bound = self
                        .state
                        .bound
                        .as_ref()
                        .ok_or_else(|| anyhow!("workspace not enrolled"))?;
                    let mut transport = Vec::new();
                    // An installed HTTPS delivery descriptor names the one exact
                    // origin; the locator is inert discovery, never authority.
                    if let Some(origin) = self
                        .state
                        .delivery
                        .as_ref()
                        .and_then(|delivery| delivery.transport_hint())
                        .map(|hint| {
                            tilo_delivery_protocol::delivery_v3::descriptor::inspect_pinned_hint_scope(
                                hint,
                                &bound.authority,
                            )
                        })
                        .transpose()?
                        .map(|scope| scope.origin)
                    {
                        transport.push(
                            tilo_delivery_protocol::workspace_join::DeliveryOriginLocatorV1 {
                                version: 1,
                                origin,
                            }
                            .transport_hint(&bound.authority, &self.key())
                            .map_err(|error| anyhow!(error.to_string()))?,
                        );
                    }
                    let packet = match std::env::var("TILO_CLOUD_PACKET_RELAY") {
                        Ok(packet) => Some(packet),
                        Err(_) if cfg!(test) && transport.is_empty() => {
                            Some("https://example.invalid".into())
                        }
                        Err(error) if transport.is_empty() => return Err(error.into()),
                        Err(_) => None,
                    };
                    if let Some(packet) = packet {
                        let packet_url: iroh_base::RelayUrl = packet.parse()?;
                        let service_public_key =
                            tilo_relay::daemon::relay_service_node_id(&self.key());
                        let endpoint_addr = iroh_base::EndpointAddr::new(
                            iroh_base::PublicKey::from_bytes(&service_public_key)?,
                        )
                        .with_relay_url(packet_url.clone());
                        let descriptor = tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
                            version: 1,
                            mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode::CertifiedHost,
                            room: self.state.room.clone(),
                            workspace_id: tilo_relay::room_workspace_id(&self.state.room)?,
                            governing_authority: peer.authority().clone(),
                            endpoint_addr,
                            host_user_public_key: bound.root,
                            host_device_certificate: bound.authority.clone(),
                            packet_relay_url: packet_url.to_string(),
                        };
                        let hint = descriptor
                            .transport_hint(&self.key())
                            .map_err(|error| anyhow!(error.to_string()))?;
                        transport.push(hint);
                    }
                    let mut nonce = [0; 16];
                    rand::rand_core::UnwrapErr(rand::rngs::SysRng).fill_bytes(&mut nonce);
                    let signed = tilo_peer_state::join::issue_invitation(
                        tilo_peer_state::join::InviteGrant {
                            workspace_id: tilo_relay::room_workspace_id(&self.state.room)?,
                            room: self.state.room.clone(),
                            authority: peer.authority().clone(),
                            issuer: peer.authority().clone(),
                            delegation: None,
                            role: *role,
                            nonce,
                            recipient_user_public_key: *recipient_user_public_key,
                            invitee_name: invitee_name.clone(),
                            expires_at_ns: *expires_at_ns,
                            transport,
                        },
                        &self.key(),
                    )
                    .map_err(anyhow::Error::msg)?;
                    let invite_id = *blake3::hash(&signed).as_bytes();
                    state.invitations.insert(
                        hex::encode(invite_id),
                        collaboration::StoredInvitation {
                            signed_invitation: signed.clone(),
                            role: *role,
                            invitee_name: invitee_name.clone(),
                            expires_at_ns: *expires_at_ns,
                            status: collaboration::InvitationStatus::Pending,
                        },
                    );
                    audit(
                        "collaboration_invitation_issued",
                        serde_json::json!({
                            "room": self.state.room,
                            "caller_device": hex::encode(source),
                            "caller_root": hex::encode(caller.user_public_key),
                            "invocation_id": request.invocation_id,
                            "invite_id": hex::encode(invite_id),
                            "role": role,
                            "expires_at_ns": expires_at_ns,
                            "result": "committing",
                        }),
                    );
                    Some(serde_json::json!({
                        "signed_invitation": tilo_peer_state::join::encode_invitation_text(&signed),
                        "invite_id": hex::encode(invite_id),
                    }))
                }
                NativeOperation::CollaborationCancelInvitation { invite_id } => {
                    let invitation = state
                        .invitations
                        .get_mut(&hex::encode(invite_id))
                        .ok_or_else(|| anyhow!("invitation does not exist"))?;
                    if invitation.status == collaboration::InvitationStatus::Redeemed {
                        bail!("redeemed invitation cannot be revoked")
                    }
                    invitation.status = collaboration::InvitationStatus::Revoked;
                    audit(
                        "collaboration_invitation_revoked",
                        serde_json::json!({"room":self.state.room,"caller_device":hex::encode(source),"caller_root":hex::encode(caller.user_public_key),"invocation_id":request.invocation_id,"invite_id":hex::encode(invite_id),"result":"committing"}),
                    );
                    None
                }
                NativeOperation::CollaborationListInvitations => Some(serde_json::json!({
                    "generation": state.generation.to_string(),
                    "invitations": state.invitations.iter().map(|(id, invitation)| serde_json::json!({
                        "id": id,
                        "role": match invitation.role {
                            tilo_peer_state::join::MemberRole::Viewer => "viewer",
                            tilo_peer_state::join::MemberRole::Commenter => "commenter",
                            tilo_peer_state::join::MemberRole::Editor => "editor",
                        },
                        "expires_at_ms": invitation.expires_at_ns / 1_000_000,
                        "status": invitation.status,
                    })).collect::<Vec<_>>(),
                })),
                NativeOperation::CollaborationSetRole {
                    user_public_key,
                    role,
                } => {
                    peer.set_member_role(
                        *user_public_key,
                        *role,
                        &self.key(),
                        now_time(self.policy_now_or(now)?),
                    )
                    .map_err(anyhow::Error::msg)?;
                    audit(
                        "collaboration_member_role_changed",
                        serde_json::json!({"room":self.state.room,"caller_device":hex::encode(source),"caller_root":hex::encode(caller.user_public_key),"invocation_id":request.invocation_id,"participant_id":hex::encode(user_public_key),"role":role,"result":"committing"}),
                    );
                    None
                }
                NativeOperation::CollaborationRemove { user_public_key } => {
                    peer.remove_member(
                        *user_public_key,
                        &self.key(),
                        now_time(self.policy_now_or(now)?),
                    )
                    .map_err(anyhow::Error::msg)?;
                    audit(
                        "collaboration_member_removed",
                        serde_json::json!({"room":self.state.room,"caller_device":hex::encode(source),"caller_root":hex::encode(caller.user_public_key),"invocation_id":request.invocation_id,"participant_id":hex::encode(user_public_key),"result":"committing"}),
                    );
                    None
                }
                _ => bail!("unsupported collaboration operation"),
            };
            if matches!(
                request.operation,
                NativeOperation::CollaborationSetRole { .. }
                    | NativeOperation::CollaborationRemove { .. }
            ) {
                state.outbox.extend(durable_mutation_publications(
                    &mut peer,
                    &before,
                    &self.key(),
                    now_time(self.policy_now_or(now)?),
                    self.next_generation()?,
                )?);
                state
                    .bound
                    .as_mut()
                    .ok_or_else(|| anyhow!("workspace missing"))?
                    .peer = peer
                    .durable()
                    .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
            }
            let receipt = ActionReceipt {
                invocation_id: request.invocation_id.clone(),
                status: "completed".into(),
                error: None,
                output,
            };
            state.receipts.insert(scope, (digest, receipt.clone()));
            self.commit(state)?;
            audit(
                "collaboration_admin_committed",
                serde_json::json!({
                    "room": self.state.room,
                    "caller_device": hex::encode(source),
                    "caller_root": hex::encode(caller.user_public_key),
                    "invocation_id": request.invocation_id,
                    "operation": operation_name,
                    "generation": self.state.generation.to_string(),
                    "result": "committed",
                }),
            );
            return Ok(receipt);
        }
        #[cfg(test)]
        return self.fixture_document_action(
            source,
            request,
            now,
            (before, peer),
            state,
            (scope, digest),
        );
        #[cfg(not(test))]
        bail!("unsupported operation")
    }
    /// Test fixture: the owner-side document effect behind the retired bounded
    /// actions, kept so owner permission and durability tests keep a real
    /// Writer mutation.
    #[cfg(test)]
    fn fixture_document_action(
        &mut self,
        source: [u8; 32],
        request: &ActionRequest,
        now: u64,
        (before, mut peer): (StatePeer, StatePeer),
        mut state: State,
        (scope, digest): (String, String),
    ) -> Result<ActionReceipt> {
        let (app, record) = match &request.operation {
            NativeOperation::WriterInsert { .. } => {
                (word_processor::definition().app_id, crate::WRITER_RECORD)
            }
            _ => bail!("unsupported operation"),
        };
        #[cfg(test)]
        policy_time_tests::before_document();
        let now = self.policy_now_or(now)?;
        let (_, caps, _person, actor) = action_permissions(&peer, source, request, now)?;
        let app_actor = actor.with_principal(Principal::App(app.into()));
        let artifact = ResourceRef::Artifact {
            app: Some(app.into()),
            kind: collab_types::ArtifactKind::Note,
            instance: collab_types::document_artifact_id(app, &self.state.room, record),
        };
        if decide_capability(
            &app_actor,
            &artifact,
            Verb::Write,
            &caps,
            Some(now / 1_000_000_000),
        ) != CapDecision::Allow
        {
            bail!("native document capability forbidden")
        }
        let result = execute(&mut peer, &self.key(), source, request, now);
        let receipt = match result {
            Ok(live_document_delta) => {
                state
                    .outbox
                    .push(StoredPublication::mutation(live_document_delta)?);
                state.outbox.extend(durable_mutation_publications(
                    &mut peer,
                    &before,
                    &self.key(),
                    now_time(self.policy_now_or(now)?),
                    self.next_generation()?,
                )?);
                state
                    .bound
                    .as_mut()
                    .ok_or_else(|| anyhow!("workspace missing"))?
                    .peer = peer
                    .durable()
                    .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
                ActionReceipt {
                    invocation_id: request.invocation_id.clone(),
                    status: "completed".into(),
                    error: None,
                    output: None,
                }
            }
            Err(e) => ActionReceipt {
                invocation_id: request.invocation_id.clone(),
                status: "failed".into(),
                output: None,
                error: Some(e.to_string()),
            },
        };
        state.receipts.insert(scope, (digest, receipt.clone()));
        self.commit(state)?;
        Ok(receipt)
    }
}

fn action_permissions(
    peer: &StatePeer,
    source: [u8; 32],
    request: &ActionRequest,
    now: u64,
) -> Result<(
    collab_types::Role,
    Vec<collab_types::caps::CapOverride>,
    String,
    Actor,
)> {
    let (role, caps, person) = peer
        .permissions(source, now_time(now))
        .map_err(anyhow::Error::msg)?;
    let actor = Actor::person(Some(role), person.as_str()).in_room(request.room.clone());
    let verb = if media::operation(&request.operation).is_some() {
        Verb::Admin
    } else if matches!(
        request.operation,
        NativeOperation::CollaborationListInvitations
    ) {
        Verb::Read
    } else {
        Verb::Write
    };
    if decide_capability(
        &actor,
        &ResourceRef::Room(request.room.clone()),
        verb,
        &caps,
        Some(now / 1_000_000_000),
    ) != CapDecision::Allow
    {
        bail!("native action write forbidden")
    }
    Ok((role, caps, person, actor))
}

#[cfg(test)]
fn execute(
    peer: &mut StatePeer,
    key: &SigningKey,
    source: [u8; 32],
    r: &ActionRequest,
    now: u64,
) -> Result<NeighbourMsg> {
    let mut registry = ActionRegistry::new();
    word_processor::actions::register(&mut registry);
    let (app, record) = match &r.operation {
        NativeOperation::WriterInsert { .. } => {
            (word_processor::definition().app_id, crate::WRITER_RECORD)
        }
        _ => bail!("unsupported operation"),
    };
    let bytes = peer
        .document(app, record)
        .ok_or_else(|| anyhow!("canonical record missing"))?;
    let mut doc = automerge::AutoCommit::load(&bytes)?;
    let heads = doc.get_heads();
    let encoded = match &r.operation {
        NativeOperation::WriterInsert { text } => {
            if text.len() > 64 * 1024 {
                bail!("text limit exceeded")
            };
            let model =
                qdoc::model::open(&doc, &automerge::ROOT).map_err(|e| anyhow!(e.to_string()))?;
            let block = qdoc::model::blocks_in_order(&doc, &model)
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("Writer body empty"))?;
            word_processor::actions::encode(&word_processor::actions::RichTextAction::SpliceText {
                region: word_processor::actions::RegionKind::Body,
                block,
                at: 0,
                del: 0,
                ins: text.clone(),
            })
        }
        _ => bail!("unsupported operation"),
    };
    let action = registry.decode(&encoded)?;
    // The native executor supplies deterministic envelope/commit semantics and attribution.
    tilo_actions::apply_local_by(&mut doc, action.as_ref(), encoded, Some(&source))?;
    peer.publish_incremental(app, record, doc.save_after(&heads), key, now_time(now))
        .map_err(anyhow::Error::msg)
}
