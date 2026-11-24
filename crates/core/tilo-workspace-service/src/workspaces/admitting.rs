//! Admission to a workspace: its one action, `workspace/members#admit`
//! ([`WORKSPACE_MEMBER_ADMITTED`]), served on this device's dispatch like every action of the
//! workspace's governance tree, so it shows in history, is decided by grants and is open to an
//! agent holding them.
//!
//! It admits a person two ways. A member whose grants let them place people names the person
//! by their key ([`Workspaces::add_member`]). A device that holds an invitation redeems it
//! ([`ADMISSIONS`]): on the connection that carries its entry, under the owner's pre-admission
//! when the invitation was issued under one, or by answering the join exchange's challenge. It
//! calls as the invitation's authority ([`Caller::invited`]), which its issuer's grant decides
//! when the action runs; the host's own pass over the redemptions a pre-admission queued calls
//! as this device ([`Caller::local_system`]) with every one of them, which the owner's
//! pre-admissions decide.
//!
//! Whichever way, the change that admits people commits, with the signed member list, each
//! one's membership record (`so.tilo.membership/member.<person>`), the invitations they spent
//! and the workspace's receipts of their requests: all of it, or none.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionCallError, Caller, Invited, InvokeArgument, InvokeRequest, MAX_REMOTE_WAIT,
    ReceiptStatus, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_MEMBER_ADMITTED,
    WORKSPACE_MEMBERS, workspace_governance_child,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_peer_state::join::{JoinBootstrap, JoinProof, VerifiedRedemption};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::{JoinReply, Role, WorkspaceNotice};
use tilo_user_identity::DeviceCert;

use super::authority::WorkspaceOp;
use super::collaboration::{
    AddMemberRequest, CollaborationError, GovernanceCompletion, HeldWorkspace, Keeping, Redeeming,
    request_digest,
};
use super::pages::Presented;
use super::{GovernedWorkspace, Workspaces, system_time};

/// The argument of `workspace/members#admit` that redeems invitations: the admissions, each a
/// [`Admitting`], answered in order with a [`Admitted`] each.
pub(super) const ADMISSIONS: &str = "admissions";

/// What the holder of an invitation reads when its issuer may no longer let them in.
const ISSUER_ADMITS_NO_MORE: &str = "invitation-issuer-admits-no-more";

/// One device admitted with its invitation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Admitting {
    /// A device redeeming `invitation` on the connection that carries its entry, on which its
    /// host checked it holds `certificate`'s key ([`tilo_server_protocol::entering`]), shown to
    /// the workspace's people as `name`, with the owner's pre-admission `token` where the
    /// invitation was issued under one.
    Entry {
        invitation: Vec<u8>,
        certificate: DeviceCert,
        request_id: [u8; 32],
        name: String,
        token: Option<Vec<u8>>,
    },
    /// A device that answered the join exchange's challenge with `proof`.
    Answered { proof: JoinProof },
}

impl Admitting {
    /// Whether this redeems `invitation` for `device`, the key of the connection calling.
    fn redeems(&self, invitation: &[u8], device: Option<[u8; 32]>) -> bool {
        match self {
            Self::Entry {
                invitation: redeemed,
                certificate,
                token: None,
                ..
            } => redeemed == invitation && device == Some(certificate.device_public_key),
            Self::Entry { token: Some(_), .. } => false,
            // The exchange's log checks the proof against the connection that answered.
            Self::Answered { proof } => {
                proof.challenge.signed_invitation == invitation && device.is_some()
            }
        }
    }

    /// Whether the owner's pre-admission decides this one.
    const fn pre_admitted(&self) -> bool {
        matches!(self, Self::Entry { token: Some(_), .. })
    }
}

/// What one [`Admitting`] came to.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Admitted {
    /// The device that entered is in: what its entry answers.
    Entered(JoinReply),
    /// The device that answered the challenge is in: the workspace as the admission committed it.
    Joined(JoinBootstrap),
    /// Refused: why, and the code the join exchange answers it with
    /// ([`CollaborationError::code`]).
    Refused { reason: String, code: u16 },
}

impl From<&CollaborationError> for Admitted {
    fn from(error: &CollaborationError) -> Self {
        Self::Refused {
            reason: error.to_string(),
            code: error.code(),
        }
    }
}

/// One admission checked, waiting for the change that admits it.
struct Checked {
    /// Where its answer goes among the admissions asked.
    index: usize,
    remote_endpoint: [u8; 32],
    proof_digest: [u8; 32],
    redemption: VerifiedRedemption,
    name: Option<String>,
    way: Way,
}

/// How a checked admission came, which decides what it is answered.
enum Way {
    Entered,
    PreAdmitted {
        label: String,
        role: Role,
        name: String,
    },
    Answered,
}

/// What admitting people to a workspace writes besides its member list: each
/// one's membership ([`super::membership::Membership`]) at the role the
/// admitting change gives them, kept in that same change, so the workspace
/// never lists a member whose membership it does not keep, nor the reverse.
struct Admission {
    /// Who the change may admit, with their membership records.
    members: Vec<([u8; 32], super::authored::HeldRecord)>,
    /// The device that writes the records: this host.
    writer: DeviceCert,
}

impl Admission {
    /// Keep in `candidate` the membership of each person it may admit at the
    /// role `candidate`'s governance gives them, where their record holds
    /// another, signed with `key` at `now`. Answers whose it kept.
    ///
    /// # Errors
    /// The governance does not read, or a record cannot be written.
    fn keep(
        &self,
        candidate: &mut tilo_peer_state::StatePeer,
        (key, now): (&ed25519_dalek::SigningKey, std::time::SystemTime),
    ) -> Result<(Vec<[u8; 32]>, Vec<neighbour_wire::NeighbourMsg>), String> {
        use super::membership::Membership;
        if self.members.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let policy = candidate
            .governance()
            .governed_policy()
            .map_err(|error| error.to_string())?;
        let made_at_ms = i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX);
        let (mut kept, mut written) = (Vec::new(), Vec::new());
        for (person, record) in &self.members {
            let Some(role) = policy.active_role(&hex::encode(person)) else {
                continue;
            };
            let role = role.key().to_string();
            let genesis = super::person_workspace::genesis::<Membership>(record.workspace())?;
            let (_, wrote) = record.write(
                candidate,
                Some(Box::new(genesis)),
                |doc| {
                    let held = tilo_resource::Replica::<Membership>::from_doc(doc.clone())
                        .state()
                        .map_err(|error| error.to_string())?;
                    if held.role.as_deref() == Some(role.as_str()) {
                        return Ok(());
                    }
                    super::person_workspace::invoke(
                        doc,
                        &self.writer,
                        Membership::KEEP_ROLE.with((role.clone(), made_at_ms)),
                    )
                },
                (key, now),
            )?;
            if !wrote.is_empty() {
                kept.push(*person);
                written.extend(wrote);
            }
        }
        Ok((kept, written))
    }
}

/// Answer the `index`th admission asked with `admitted`.
fn answer(answers: &mut [Option<Admitted>], index: usize, admitted: Admitted) {
    if let Some(slot) = answers.get_mut(index) {
        *slot = Some(admitted);
    }
}

/// Whether `request` redeems invitations ([`ADMISSIONS`]) rather than naming a person by key.
pub(super) fn redeems(request: &InvokeRequest) -> bool {
    matches!(&request.argument, InvokeArgument::Json(arguments) if arguments.contains_key(ADMISSIONS))
}

impl Workspaces {
    /// Hold the person `request.user_public_key` in `workspace` at
    /// `request.place`, whether or not a device of theirs has joined: what an
    /// administrator does when they know the person's key rather than handing
    /// them an invitation. A member already is moved to the place; their
    /// entry lets them in from then on, and a key of theirs reaches the
    /// workspace before any device of theirs has joined.
    ///
    /// # Errors
    ///
    /// Why `administrator`'s person may not give that place, a name that is not
    /// a display name, the workspace's own owner, or governance that cannot be
    /// updated.
    pub async fn add_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: AddMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let person = request.user_public_key;
        let place = request.place;
        let op = WorkspaceOp::Place(person, place);
        self.authorize(administrator, Some(&workspace), op, now)?;
        let key = self.inner.host.key.clone();
        let name = request.name.as_deref();
        let admission = self.admission(&workspace, [person]).await?;
        let mut kept = (Vec::new(), Vec::new());
        let done = self
            .administer(
                &mut workspace,
                request.request_id,
                request_digest(&("member", person, &place, name))?,
                |peer| {
                    let changed = peer.governance_mut().place_member(
                        person,
                        place,
                        name,
                        &key,
                        system_time(now),
                    )?;
                    kept = admission.keep(peer, (&key, system_time(now)))?;
                    Ok(changed)
                },
                now,
            )
            .await;
        if done.is_ok() {
            self.carry_written(workspace.workspace_id(), &kept.1)?;
            self.kept_memberships(workspace.workspace(), &kept.0);
        }
        drop(workspace);
        done
    }

    /// `workspace/members#admit` with invitations ([`ADMISSIONS`]), asked by `caller` of the
    /// workspace `workspace_id`: answers what each admission came to, in order.
    ///
    /// # Errors
    /// A caller who neither holds the invitation it redeems nor is this device's own pass over
    /// pre-admitted redemptions, admissions that do not read, or a change that cannot be made.
    pub(super) async fn admit_invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err(ActionCallError::UnknownAction);
        };
        let admissions: Vec<Admitting> =
            serde_json::from_value(arguments.get(ADMISSIONS).cloned().unwrap_or(Value::Null))
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let device = caller.device_public_key().copied();
        let invited = caller.invitation();
        // An invitation admits the one device that holds it; only this device's own pass admits
        // many at once, each under the owner's pre-admission.
        let allowed = match invited {
            Some(invited) => {
                admissions.len() == 1
                    && admissions
                        .iter()
                        .all(|admission| admission.redeems(&invited.invitation, device))
            }
            None => caller.is_system() && admissions.iter().all(Admitting::pre_admitted),
        };
        if !allowed {
            return Err(ActionCallError::UnknownAction);
        }
        let issuer = invited.map(|invited| invited.issuer);
        let asked = admissions.len();
        // Each admission is answered with what became of it, a refusal with its code.
        let admitted = self
            .admit_all(workspace_id, (issuer, device), admissions)
            .await
            .unwrap_or_else(|error| vec![Admitted::from(&error); asked]);
        serde_json::to_value(admitted).map_err(|error| ActionCallError::refused(error.to_string()))
    }

    /// Admit `admissions` to the workspace `workspace_id` in one change: what
    /// [`Self::admit_invoke`] runs, as the invitation `issuer` signed for the calling `device`
    /// (none for this device's own pass).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked while its admissions are committed"
    )]
    async fn admit_all(
        &self,
        workspace_id: WorkspaceId,
        (issuer, device): (Option<[u8; 32]>, Option<[u8; 32]>),
        admissions: Vec<Admitting>,
    ) -> Result<Vec<Admitted>, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let governed = self.workspace(&workspace_id.to_string()).await?;
        let mut workspace = governed.lock().await;
        let mut answers: Vec<Option<Admitted>> = vec![None; admissions.len()];
        let mut checked = Vec::new();
        let mut invites = std::collections::BTreeSet::new();
        for (index, admission) in admissions.into_iter().enumerate() {
            match self.check(
                &workspace,
                index,
                admission,
                (issuer, device),
                &mut invites,
                now,
            ) {
                Ok(Ok(admitting)) => checked.push(admitting),
                Ok(Err(answered)) => answer(&mut answers, index, answered),
                Err(error) => answer(&mut answers, index, Admitted::from(&error)),
            }
        }
        let joins: Vec<Redeeming<'_>> = checked
            .iter()
            .map(|checked| Redeeming {
                remote_endpoint: checked.remote_endpoint,
                request_id: checked.redemption.request_id(),
                proof_digest: checked.proof_digest,
                redemption: &checked.redemption,
                name: checked.name.as_deref(),
            })
            .collect();
        let admitted = if joins.is_empty() {
            Ok(Vec::new())
        } else {
            self.admit_redemptions(&mut workspace, &joins, now).await
        };
        drop(joins);
        let mut spent = Vec::new();
        let mut pre_admitted = Vec::new();
        match admitted {
            Ok(admitted) => {
                for (checked, admitted) in checked.into_iter().zip(admitted) {
                    let answer = match admitted {
                        Ok(bootstrap) => self.answer(&workspace, &checked, bootstrap, now),
                        Err(error) => Err(error),
                    };
                    if answer.is_ok()
                        && let Way::PreAdmitted { label, role, name } = &checked.way
                    {
                        pre_admitted.push(checked.index);
                        spent.push((
                            checked.redemption.join().invite_id(),
                            WorkspaceNotice::PreAdmitted {
                                user_public_key: checked.redemption.certificate().user_public_key,
                                name: name.clone(),
                                label: label.clone(),
                                role: *role,
                                at_ns: now,
                            },
                        ));
                    }
                    let admitted = answer.unwrap_or_else(|error| Admitted::from(&error));
                    self::answer(&mut answers, checked.index, admitted);
                }
            }
            Err(error) => {
                for checked in checked {
                    answer(&mut answers, checked.index, Admitted::from(&error));
                }
            }
        }
        if !spent.is_empty()
            && let Err(error) = workspace.admissions.spend(spent)
        {
            let error = CollaborationError::from(error);
            for index in pre_admitted {
                answer(&mut answers, index, Admitted::from(&error));
            }
        }
        Ok(answers
            .into_iter()
            .map(|answer| {
                answer.unwrap_or_else(|| Admitted::from(&CollaborationError::new("not answered")))
            })
            .collect())
    }

    /// Check `admission`, the `index`th asked, against `workspace` as it stands: what admits it,
    /// or what it is answered at once (a request asked again), or why it is refused. `invites`
    /// holds the invitations already admitted in this change.
    fn check(
        &self,
        workspace: &GovernedWorkspace,
        index: usize,
        admission: Admitting,
        (issuer, device): (Option<[u8; 32]>, Option<[u8; 32]>),
        invites: &mut std::collections::BTreeSet<[u8; 32]>,
        now: u64,
    ) -> Result<Result<Checked, Admitted>, CollaborationError> {
        let answering = self.device_cert();
        let (remote_endpoint, proof_digest, redemption, name, way) = match admission {
            Admitting::Entry {
                invitation,
                certificate,
                request_id,
                name,
                token,
            } => {
                let presented = Presented {
                    invitation,
                    cert: certificate,
                    request_id,
                };
                let device = presented.device();
                // A page that asked again under the same request (a reload, a lost answer)
                // hears what became of it, not that its invitation is spent.
                if token.is_none() && workspace.admitted_device(request_id) == Some(device) {
                    return Ok(Err(Admitted::Entered(Self::admitted(
                        workspace, device, now,
                    )?)));
                }
                let redemption = presented.redeem(workspace, answering, now)?;
                let way = match token {
                    Some(token) => {
                        let role = redemption.join().grant().role;
                        let grant = workspace.admissions.grant_for(
                            redemption.join().invite_id(),
                            &token,
                            now,
                        )?;
                        self.pre_admission_signer(workspace, grant)?;
                        if grant.role != role || grant.workspace != workspace.workspace() {
                            return Err(CollaborationError::new(
                                "the pre-admission is for another workspace or role",
                            ));
                        }
                        Way::PreAdmitted {
                            label: grant.label.clone(),
                            role,
                            name: name.clone(),
                        }
                    }
                    None => Way::Entered,
                };
                // A device that names no person arrives as it is: the name the workspace
                // already knows the person by stands, and one it does not know yet stays
                // unnamed rather than taking a device's label for a person's name.
                let named = match way {
                    Way::PreAdmitted { .. } => Some(name),
                    _ => (!name.trim().is_empty()).then_some(name),
                };
                (device, presented.digest()?, redemption, named, way)
            }
            Admitting::Answered { proof } => {
                let remote_endpoint =
                    device.ok_or_else(|| CollaborationError::new("no device answered"))?;
                let request_id = proof.challenge.request_id;
                let proof_digest = request_digest(&proof)?;
                let prior = workspace.collaboration().completed(
                    &workspace.governed(answering),
                    remote_endpoint,
                    request_id,
                    proof_digest,
                    now,
                )?;
                if let Some(prior) = prior {
                    return Ok(Err(Admitted::Joined(
                        workspace.governed(answering).filled(prior)?,
                    )));
                }
                let redemption = workspace.collaboration().redeem(
                    &workspace.governed(answering),
                    remote_endpoint,
                    &proof,
                    now,
                )?;
                (
                    remote_endpoint,
                    proof_digest,
                    redemption,
                    None,
                    Way::Answered,
                )
            }
        };
        let invite_id = redemption.join().invite_id();
        if workspace.redeemed(invite_id) || !invites.insert(invite_id) {
            return Err(CollaborationError::new("invitation was already redeemed"));
        }
        if !matches!(way, Way::PreAdmitted { .. }) {
            // An invitation is the approval of whoever made it: a device that holds one is let
            // in by its issuer's own grant on letting people in (`member.add`), decided now, and
            // never waits for someone in the workspace to answer for it.
            let grant = redemption.join().grant();
            if issuer != Some(grant.issuer.device_public_key) {
                return Err(CollaborationError::new(
                    "the invitation was issued by another key",
                ));
            }
            self.authorize(
                &grant.issuer,
                Some(workspace),
                WorkspaceOp::Admit(grant.role),
                now,
            )
            .map_err(|_| {
                CollaborationError::new(crate::i18n::message(
                    None,
                    ISSUER_ADMITS_NO_MORE,
                    &tilo_history::FluentArgs::new(),
                ))
            })?;
        }
        Ok(Ok(Checked {
            index,
            remote_endpoint,
            proof_digest,
            redemption,
            name,
            way,
        }))
    }

    /// What `checked`, admitted as `bootstrap`, is answered.
    fn answer(
        &self,
        workspace: &GovernedWorkspace,
        checked: &Checked,
        bootstrap: JoinBootstrap,
        now: u64,
    ) -> Result<Admitted, CollaborationError> {
        let certificate = checked.redemption.certificate();
        let invite_id = hex::encode(checked.redemption.join().invite_id());
        let person = hex::encode(certificate.user_public_key);
        match &checked.way {
            Way::Entered => {
                tracing::info!(
                    workspace = %workspace.workspace(),
                    %invite_id,
                    %person,
                    outcome = "admitted",
                    "workspace join admitted with its invitation"
                );
                Self::admitted(workspace, certificate.device_public_key, now).map(Admitted::Entered)
            }
            Way::PreAdmitted { role, .. } => {
                tracing::info!(
                    workspace = %workspace.workspace(),
                    %invite_id,
                    %person,
                    outcome = "pre-admitted",
                    "workspace join admitted under the owner's pre-admission"
                );
                Ok(Admitted::Entered(JoinReply::Admitted {
                    workspace: workspace.workspace().to_owned(),
                    workspace_id: workspace.workspace_id(),
                    name: workspace.name(),
                    role: *role,
                }))
            }
            // The device is sent the workspace as the admission committed it.
            Way::Answered => workspace
                .governed(self.device_cert())
                .filled(bootstrap)
                .map(Admitted::Joined),
        }
    }

    /// Admit the verified `joins` to `workspace` in one governance change: the
    /// shared admission ([`tilo_peer_state::join_answer::admit`]), each
    /// person's membership record kept in that change ([`Admission`]),
    /// published before anyone hears their answer. People arriving together
    /// cost the workspace one write and one publication instead of one each,
    /// and each of those grows with the workspace.
    ///
    /// Answers each join in order. Fails as a whole when the change cannot
    /// be committed or published.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the replica stays held until the admission is committed over it"
    )]
    async fn admit_redemptions(
        &self,
        workspace: &mut GovernedWorkspace,
        joins: &[Redeeming<'_>],
        now: u64,
    ) -> Result<Vec<Result<JoinBootstrap, CollaborationError>>, CollaborationError> {
        let key = &self.inner.host.key;
        let people: Vec<[u8; 32]> = joins
            .iter()
            .map(|join| join.redemption.certificate().user_public_key)
            .collect();
        let admission = self.admission(workspace, people).await?;
        let deciding = crate::timing::start();
        let (admitted, committed, kept, written) = {
            let keep: Keeping<'_> = &|peer, at| admission.keep(peer, at);
            let shared = workspace.peer.shared();
            let mut replica = shared.lock();
            let mut held = HeldWorkspace::new(
                key,
                self.device_cert(),
                workspace,
                &mut replica,
                Some(keep),
            );
            let admitted = tilo_peer_state::join_answer::admit(&mut held, joins, key, now);
            (admitted, held.committed, held.kept, held.written)
        };
        crate::timing::event("join.admit.decide", deciding, joins.len() as u64);
        if committed {
            let settling = crate::timing::start();
            self.settle(workspace).await?;
            crate::timing::event("join.admit.settle", settling, joins.len() as u64);
            self.carry_written(workspace.workspace_id(), &written)?;
            self.kept_memberships(workspace.workspace(), &kept);
            // The workspace keeps the grant of every key it admitted, so every
            // holder and every API server reads the same grant from the
            // workspace itself, whichever way the key came in.
            if let Ok(answers) = &admitted {
                for (join, answer) in joins.iter().zip(answers) {
                    let certificate = join.redemption.certificate();
                    if answer.is_ok()
                        && let Some(scope) = certificate.capabilities.agent
                    {
                        self.list_grant(workspace, certificate, scope, join.name, now)
                            .await?;
                    }
                }
            }
            // Someone invited from the wait list is in now: their entry
            // leaves it.
            workspace.forget_joined_wait_list();
        }
        admitted
    }

    /// Take what admitting `people` to `workspace` writes of their memberships.
    ///
    /// # Errors
    /// A membership record cannot be written here
    /// ([`Self::hold_record`]).
    async fn admission(
        &self,
        workspace: &GovernedWorkspace,
        people: impl IntoIterator<Item = [u8; 32]>,
    ) -> Result<Admission, CollaborationError> {
        let mut members: Vec<([u8; 32], super::authored::HeldRecord)> = Vec::new();
        for person in people {
            if members.iter().any(|(held, _)| *held == person) {
                continue;
            }
            let record = super::membership::member_record(&person);
            let held = self
                .hold_record(workspace, (super::membership::APP, &record))
                .await?;
            members.push((person, held));
        }
        Ok(Admission {
            members,
            writer: self.device_cert().clone(),
        })
    }

    /// Tell whoever follows `workspace`'s records of the memberships an
    /// admission kept of `people`.
    fn kept_memberships(&self, workspace: &str, people: &[[u8; 32]]) {
        for person in people {
            self.wrote(
                workspace,
                super::membership::APP,
                &super::membership::member_record(person),
            );
        }
    }

    /// Ask `workspace/members#admit` of the workspace `workspace_id` on this device's dispatch,
    /// as `caller`, with `admissions`: what each came to, in order.
    ///
    /// # Errors
    /// The dispatch serves no governance here, or refused or failed the call.
    pub(super) async fn admit_through(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        admissions: Vec<Admitting>,
    ) -> Result<Vec<Admitted>, CollaborationError> {
        let refused = |error: String| CollaborationError::new(error);
        let mut arguments = Map::new();
        arguments.insert(
            ADMISSIONS.to_owned(),
            serde_json::to_value(admissions).map_err(|error| refused(error.to_string()))?,
        );
        let workspace = workspace_id.to_string();
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: ActionAddress::new(
                    workspace_governance_child(&workspace, Some(WORKSPACE_MEMBERS)).as_str(),
                    WORKSPACE_MEMBER_ADMITTED,
                ),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(u32::try_from(MAX_REMOTE_WAIT.as_millis()).unwrap_or(u32::MAX)),
            appearance: None,
            signed: None,
        };
        let invocation = request.invocation;
        let providers = self.providers();
        let receipt = providers
            .invoke(caller, request)
            .ok_or_else(|| refused("this device admits no one to its workspaces".to_owned()))?
            .await
            .ok_or_else(|| refused("the admission stopped before it answered".to_owned()))?
            .map_err(|error| refused(error.to_string()))?;
        let output = match receipt.status {
            ReceiptStatus::Completed { output } => output,
            ReceiptStatus::Accepted => {
                // Past the dispatch's wait the admission still runs: its answer is followed.
                let governing = providers
                    .get(WORKSPACE_GOVERNANCE_PROVIDER)
                    .ok_or_else(|| refused("the admission went away".to_owned()))?
                    .answers;
                loop {
                    if let Some(state) = governing.observe(caller, &invocation) {
                        match state.update {
                            InvocationUpdate::Succeeded { output, .. } => break output,
                            InvocationUpdate::Failed { error, .. } => {
                                return Err(refused(error.message));
                            }
                            InvocationUpdate::Cancelled { .. } => {
                                return Err(refused("the admission was cancelled".to_owned()));
                            }
                            _ => {}
                        }
                    }
                    match governing.next_change(caller, &invocation) {
                        Some(change) => {
                            let _ = change.await;
                        }
                        None => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                    }
                }
            }
            ReceiptStatus::Failed { error } => return Err(refused(error.message)),
            ReceiptStatus::Rejected { error } => return Err(refused(error.to_string())),
            ReceiptStatus::Cancelled { .. } => {
                return Err(refused("the admission was cancelled".to_owned()));
            }
        };
        serde_json::from_value(output.value.unwrap_or(Value::Null))
            .map_err(|error| refused(error.to_string()))
    }

    /// Admit, through `workspace/members#admit`, the device at `remote_endpoint` that answered
    /// the join exchange's challenge with `proof`, as the invitation it redeems: the workspace as
    /// the admission committed it.
    ///
    /// # Errors
    /// The invitation does not hold, or the admission refused it.
    pub(super) async fn admit_answered(
        &self,
        remote_endpoint: [u8; 32],
        proof: JoinProof,
        now: u64,
    ) -> Result<JoinBootstrap, CollaborationError> {
        let invitation = proof.challenge.signed_invitation.clone();
        let issuer = tilo_peer_state::join_log::invitation_issuer(&invitation, now)?;
        let caller = Caller::invited(remote_endpoint, Invited { invitation, issuer });
        let workspace = proof.challenge.workspace_id;
        match self
            .admit_through(workspace, &caller, vec![Admitting::Answered { proof }])
            .await?
            .pop()
        {
            Some(Admitted::Joined(bootstrap)) => Ok(bootstrap),
            Some(Admitted::Refused { reason, code }) => {
                Err(CollaborationError::coded(reason, code))
            }
            _ => Err(CollaborationError::new("join bootstrap was not built")),
        }
    }
}

impl Admitted {
    /// What a device that entered with its invitation is answered.
    pub(super) fn entered(self) -> JoinReply {
        match self {
            Self::Entered(reply) => reply,
            Self::Refused { reason, .. } => JoinReply::Refused(reason),
            Self::Joined(_) => JoinReply::Refused("the join was answered another way".to_owned()),
        }
    }
}
