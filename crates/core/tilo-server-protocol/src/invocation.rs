//! Signing an invocation as its caller, and checking one as a host.
//!
//! [`SignedInvocation`] is the caller's statement of who asks for which action
//! on which resource of which workspace, with which arguments. A device signs
//! each invocation it sends a host with its own key ([`sign_invocation`]);
//! the host runs it as the verified signer ([`verify_invocation`]) and never
//! signs one in a caller's place. A change the host makes for it keeps it, and
//! a peer reading the change checks it there ([`verify_recorded`]). What the envelope proves, and what it does
//! not, is in docs/security/README.md ("Signed invocations").

// A page signs through its own key, a JavaScript object bound to its one thread, so the futures
// that await a signature never move to another thread.
#![cfg_attr(
    target_family = "wasm",
    allow(
        clippy::future_not_send,
        reason = "a page's signer is a JavaScript object and its signing futures run on the page's one thread"
    )
)]

use collab_types::signer::DeviceSigner;
use tilo_actions::remote::signed_invocation::{
    INVOCATION_SIGNATURE_TOLERANCE_NS, InvocationStatement, MAX_SIGNER_CERT_BYTES,
    StatementMismatch,
};
use tilo_actions::remote::{InvokeRequest, RemoteActionMessage};
use tilo_user_identity::{DeviceCert, DeviceCertError, RevocationList, verify_device_cert_at};

pub use tilo_actions::remote::SignedInvocation;

/// Why an invocation was not signed, or its signature not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvocationError {
    /// The request carries no signature, where one is required.
    #[error("the invocation is not signed by its caller")]
    Unsigned,
    /// The carried certificate does not decode, or is too large.
    #[error("the signed invocation's certificate does not decode")]
    Encoding,
    /// The certificate certifies another key than the one that signed.
    #[error("the signed invocation's certificate is another key's")]
    SignerMismatch,
    /// The signature does not verify under the signer's key.
    #[error("the invocation's signature does not verify")]
    Signature,
    /// The signature covers another request than the one sent with it.
    #[error("{0}")]
    Mismatch(#[from] StatementMismatch),
    /// The issue time is outside the freshness window.
    #[error("the signed invocation is outside its freshness window")]
    Stale,
    /// The signer's certificate does not verify, or is expired or not yet
    /// valid.
    #[error("the signer's certificate is not valid: {0}")]
    Certificate(DeviceCertError),
    /// The signer's person revoked its key.
    #[error("the signer's key is revoked")]
    Revoked,
    /// The signer is a grant that does not reach the workspace.
    #[error("the signer's grant does not reach this workspace")]
    OutOfScope,
    /// The same invocation id was signed for another request.
    #[error("the invocation id was already used for another request")]
    Replayed,
    /// The device's signer failed.
    #[error("the device signer failed: {0}")]
    Signer(String),
    /// A change keeps an invocation another person signed than the one it
    /// names as its invoker.
    #[error("the invocation a change keeps is signed by someone it does not name")]
    NotInvoker,
    /// An agent acts, but the key that signed is not a key its person certified for an agent:
    /// a device's own key, or a key that acts as the person.
    #[error("an agent acts here without its own certified key")]
    NotAgentSigned,
}

impl InvocationError {
    /// Stable code a host refuses with.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsigned => "invocation_unsigned",
            Self::Encoding
            | Self::SignerMismatch
            | Self::Signature
            | Self::Mismatch(_)
            | Self::Certificate(_) => "invocation_signature_invalid",
            Self::Stale => "invocation_stale",
            Self::Revoked => "invocation_signer_revoked",
            Self::OutOfScope => "invocation_out_of_scope",
            Self::Replayed => "invocation_replayed",
            Self::Signer(_) => "invocation_signer_failed",
            Self::NotInvoker => "invocation_not_invoker",
            Self::NotAgentSigned => "invocation_not_agent_signed",
        }
    }
}

/// `request` in `workspace`, signed at `now_ns` by the key `cert` certifies,
/// which `signer` holds. Awaits the signer: a browser's key signs through a
/// promise.
///
/// # Errors
///
/// [`InvocationError::SignerMismatch`] when `signer` holds another key than
/// `cert`'s, [`InvocationError::Encoding`] for a certificate that does not
/// encode within its bound, [`InvocationError::Signer`] when the signer fails.
pub async fn sign_invocation(
    workspace: &str,
    request: &InvokeRequest,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<SignedInvocation, InvocationError> {
    let (statement, encoded_cert) = statement_for(workspace, request, cert, signer, now_ns)?;
    let signature = signer
        .sign(statement.signing_message())
        .await
        .map_err(|error| InvocationError::Signer(error.0))?;
    Ok(SignedInvocation::new(statement, signature, encoded_cert))
}

/// [`sign_invocation`] in this turn, for a signer that answers at once (a key
/// kept in memory). A signer that answers only later fails with
/// [`InvocationError::Signer`].
///
/// # Errors
///
/// As [`sign_invocation`].
pub fn sign_invocation_now(
    workspace: &str,
    request: &InvokeRequest,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<SignedInvocation, InvocationError> {
    let (statement, encoded_cert) = statement_for(workspace, request, cert, signer, now_ns)?;
    let signature = collab_types::signer::sign_now(signer, statement.signing_message())
        .map_err(|error| InvocationError::Signer(error.0))?;
    Ok(SignedInvocation::new(statement, signature, encoded_cert))
}

/// Sign the invocation `message` carries, if it carries one, as
/// [`sign_invocation`] does, replacing any signature it carried: a device signs
/// what it sends under its own key, never one an app handed it. Every other
/// message goes as it is.
///
/// # Errors
///
/// As [`sign_invocation`].
pub async fn sign_message(
    workspace: &str,
    message: &mut RemoteActionMessage,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<(), InvocationError> {
    if let RemoteActionMessage::Invoke(request) = message {
        request.signed = None;
        request.signed = Some(Box::new(
            sign_invocation(workspace, request, cert, signer, now_ns).await?,
        ));
    }
    Ok(())
}

/// [`sign_message`] in this turn, for a signer that answers at once.
///
/// # Errors
///
/// As [`sign_invocation_now`].
pub fn sign_message_now(
    workspace: &str,
    message: &mut RemoteActionMessage,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<(), InvocationError> {
    if let RemoteActionMessage::Invoke(request) = message {
        request.signed = None;
        request.signed = Some(Box::new(sign_invocation_now(
            workspace, request, cert, signer, now_ns,
        )?));
    }
    Ok(())
}

fn statement_for(
    workspace: &str,
    request: &InvokeRequest,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    now_ns: u64,
) -> Result<(InvocationStatement, Vec<u8>), InvocationError> {
    let key = signer.device_public_key();
    if key != cert.device_public_key {
        return Err(InvocationError::SignerMismatch);
    }
    let encoded = postcard::to_allocvec(cert).map_err(|_| InvocationError::Encoding)?;
    if encoded.len() > MAX_SIGNER_CERT_BYTES {
        return Err(InvocationError::Encoding);
    }
    Ok((
        InvocationStatement::of(workspace, request, now_ns, key),
        encoded,
    ))
}

/// Check that `request` in `workspace` is signed by its caller, and return
/// the caller's verified certificate. Checks, in order: the signature under
/// the signing key, that it covers this very request (workspace, invocation,
/// target, arguments, the invocation it continues), its freshness at
/// `now_ns`, the certificate (its chain to the person's root, an issuer's
/// included, and its validity at `now_ns`), its revocation in `revocations`,
/// and that a grant reaches the workspace. Whether the signer is a member
/// with the grants the action needs is the host's admission and policy, which
/// it decides for the certificate this returns.
///
/// # Errors
///
/// The [`InvocationError`] naming the first check that fails.
pub fn verify_invocation(
    workspace: &str,
    request: &InvokeRequest,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, InvocationError> {
    let signed = request.signed.as_ref().ok_or(InvocationError::Unsigned)?;
    check_signature(signed)?;
    signed.statement().covers(workspace, request)?;
    if !signed.statement().fresh_at(now_ns) {
        return Err(InvocationError::Stale);
    }
    signer_of(signed, workspace, revocations, (now_ns, None))
}

/// Check the invocation `request` serves ([`SignedInvocation::serves`]): a call an app makes while
/// it runs that invocation for its caller, which carries it as its proof. Checks the signature,
/// that the invocation is this workspace's, the certificate and its revocation now, and that a
/// grant reaches the workspace, and returns the original caller's verified certificate. Its issue
/// time is not checked against the freshness window: the invocation runs as long as its app takes,
/// and whether it still runs is read from its record by whoever decides the call.
///
/// # Errors
///
/// The [`InvocationError`] naming the first check that fails.
pub fn verify_served(
    workspace: &str,
    request: &InvokeRequest,
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, InvocationError> {
    let signed = request.signed.as_ref().ok_or(InvocationError::Unsigned)?;
    if !signed.serves(request) {
        return Err(InvocationError::Mismatch(StatementMismatch::OnBehalfOf));
    }
    check_signature(signed)?;
    if signed.statement().workspace != workspace {
        return Err(InvocationError::Mismatch(StatementMismatch::Workspace));
    }
    signer_of(signed, workspace, revocations, (now_ns, None))
}

/// Check the invocation a change made for it keeps (`context`'s
/// [`tilo_actions::ActionContext::signed`]), as a peer reading the change in
/// `workspace` does, and return the certificate of the caller who asked.
/// `Ok(None)` for a change that keeps none: the host made it of its own
/// accord, or for someone who asked under no key of theirs.
///
/// Checks the signature, that it is this workspace's and the change's own
/// invocation ([`tilo_actions::ActionContext::source_invocation`]), the
/// argument kept beside it against the signed hash, the certificate's chain
/// and validity when it signed, its revocation in `revocations`, a grant's
/// reach, and that the person who signed is the one the change names as its
/// invoker. Freshness is the host's to check when it runs the invocation: a
/// change is read long after.
///
/// A revocation counts from its own time against the earlier of when the
/// invocation was signed and `at_ns`, when the change was written: every device
/// that reads the change, now or years later, reaches the same verdict on it, and
/// a device revoked afterwards does not take back what it asked for before.
///
/// # Errors
///
/// The first check that fails, as [`InvocationError`]:
/// [`InvocationError::NotInvoker`] when the person who signed is none the
/// change names.
pub fn verify_recorded(
    context: &tilo_actions::ActionContext,
    (workspace, keeper): (&str, Option<&str>),
    revocations: &RevocationList,
    at_ns: u64,
) -> Result<Option<DeviceCert>, InvocationError> {
    let invoker = context.invoker.as_ref().map(|invoker| &invoker.acting);
    let Some(call) = context.signed.as_deref() else {
        // An agent with a key of its own asks under that key: a change naming one that keeps
        // no invocation it signed has nothing to show it asked.
        return match invoker.and_then(own_key_agent) {
            Some(_) => Err(InvocationError::NotAgentSigned),
            None => Ok(None),
        };
    };
    let signed = &call.signed;
    check_signature(signed)?;
    let statement = signed.statement();
    if statement.workspace != workspace {
        return Err(StatementMismatch::Workspace.into());
    }
    if context.source_invocation != Some(statement.invocation) {
        return Err(StatementMismatch::Invocation.into());
    }
    if call.argument_signed() == Some(false) {
        return Err(StatementMismatch::Arguments.into());
    }
    // The change stays inside what the call named: its resource, or the type of it a caller that
    // knows only the app names, and the action it names when the change says which verb it ran.
    // Or it is a step the called action declares it takes in another record, written by the
    // device that keeps that record. A host that ran another resource's action for the call, or
    // another action on it, is refused here.
    let named = &statement.target.address.resource;
    let in_resource = match &context.resource {
        Some(resource) => &resource.kind == named || &resource.id == named,
        None => context.what.is_none(),
    };
    let names_verb = match &context.what {
        Some(tilo_actions::Did::Verb(verb)) => {
            verb.action.action == statement.target.address.action
        }
        _ => true,
    };
    if !(in_resource && names_verb) && !declared_step(context, &statement.target, keeper) {
        return Err(StatementMismatch::Target.into());
    }
    let cert = signer_of(
        signed,
        workspace,
        revocations,
        (
            statement.issued_at_ns,
            Some(statement.issued_at_ns.min(at_ns)),
        ),
    )?;
    // The signer is a person the change names: the invoker, the person it works for, or one
    // further down its chain who bounds that work (the person an agent belongs to when it
    // answers someone else, the starter's person of a sub-agent). Each of them bounds what the
    // change may do, so naming the signer among them gives it nothing more.
    let person = hex::encode(cert.user_public_key);
    let names_signer = context
        .invoker
        .as_ref()
        .is_some_and(|invoker| invoker.acting.people().contains(&person.as_str()));
    if !names_signer {
        return Err(InvocationError::NotInvoker);
    }
    agent_signed(invoker, &cert)?;
    Ok(Some(cert))
}

/// Whether `context` records a step the action `target` names declares it takes
/// ([`tilo_actions::ActionDescriptor::effects`], read from the action's declaration, never from
/// the call), in a record of the app `keeper`: the app of the record the change is in, when the
/// device that signed the change keeps that record for the workspace. `None` for any other
/// device, which takes no step for a call but the call's own. The record's app is the one
/// matched: the app a step's history entry names is the app that wrote it, not where it is.
fn declared_step(
    context: &tilo_actions::ActionContext,
    target: &tilo_actions::remote::RemoteActionTarget,
    keeper: Option<&str>,
) -> bool {
    let (Some(app), Some(tilo_actions::Did::Verb(verb)), Some(resource)) =
        (keeper, &context.what, &context.resource)
    else {
        return false;
    };
    verb.action.kind == resource.kind && declares(target, app, &resource.kind, &verb.action.action)
}

/// Whether the action `target` names declares the step `verb` on records of `kind` in `app`.
fn declares(
    target: &tilo_actions::remote::RemoteActionTarget,
    app: &str,
    kind: &str,
    verb: &str,
) -> bool {
    tilo_actions::remote::declared_effects(target)
        .iter()
        .any(|effect| effect.app == app && effect.kind == kind && effect.verb == verb)
}

/// Check the call a sealed op's claim carries, as a receiver that keeps the
/// op without opening it does, and return the certificate of the caller who
/// asked. The call is the caller's own signed statement, so this reads no
/// arguments: only their hash is in it.
///
/// The call must be this workspace's; it must name the record the claim is of,
/// the app it belongs to, or the resource kind the claim names; every action
/// the claim names must be the one the caller asked for; its certificate must
/// verify and not be revoked; and the person who signed must be the member the
/// claim names, under the same rules a change's kept invocation is checked by
/// ([`verify_recorded`]). Freshness is not checked: a key-less device keeps and
/// forwards a sealed op long after it was written.
///
/// # Errors
///
/// The first check that fails, as [`InvocationError`].
pub fn verify_carried(
    signed: &SignedInvocation,
    (workspace, kept): (&str, bool),
    record: &collab_types::receipt::WrittenDocument<'_>,
    claim: &collab_types::claim::ChangeClaim,
    revocations: &RevocationList,
) -> Result<DeviceCert, InvocationError> {
    check_signature(signed)?;
    let statement = signed.statement();
    if statement.workspace != workspace {
        return Err(StatementMismatch::Workspace.into());
    }
    // The call names the record the change is of — or the app, or the kind of
    // it, which a caller that knows only the app names — and every action the
    // claim names is the one the caller asked for: a change claiming two verbs
    // was made for one call that named neither or only one. Or, where the
    // device that signed the change keeps the record (`kept`), every verb the
    // claim names is a step the called action declares it takes there.
    let named = &statement.target.address.resource;
    let in_record =
        named == record.document || named == record.app || claim_names_kind(claim, named);
    let verbs: Vec<&tilo_actions::ActionKey> = claim
        .action
        .members()
        .into_iter()
        .filter_map(|member| match member {
            collab_types::claim::ClaimedAction::Verb { action, .. } => Some(action),
            _ => None,
        })
        .collect();
    let names_verb = verbs
        .iter()
        .all(|action| action.action == statement.target.address.action);
    let declared = kept
        && !verbs.is_empty()
        && verbs.iter().all(|action| {
            (action.app.is_empty() || action.app == record.app)
                && declares(&statement.target, record.app, &action.kind, &action.action)
        });
    if !(in_record && names_verb) && !declared {
        return Err(StatementMismatch::Target.into());
    }
    let cert = signer_of(
        signed,
        workspace,
        revocations,
        (statement.issued_at_ns, None),
    )?;
    let person = hex::encode(cert.user_public_key);
    let invoker = claim.invoker.as_deref();
    let names_signer = invoker.is_some_and(|acting| acting.people().contains(&person.as_str()));
    if !names_signer {
        return Err(InvocationError::NotInvoker);
    }
    agent_signed(invoker, &cert)?;
    Ok(cert)
}

/// Whether the claim names `kind` as the resource kind of one of its actions.
fn claim_names_kind(claim: &collab_types::claim::ChangeClaim, kind: &str) -> bool {
    claim
        .action
        .members()
        .into_iter()
        .any(|member| match member {
            collab_types::claim::ClaimedAction::Verb { action, .. } => action.kind == kind,
            collab_types::claim::ClaimedAction::Edit { kind: named, .. } => named == kind,
            _ => false,
        })
}

/// Check the invocation a resource store's change keeps in its stamp, as a peer reading the
/// change in `workspace` does, and return the certificate of the caller who asked. `Ok(None)` for
/// a change that keeps none: the host made it of its own accord.
///
/// The change stays inside what the call named: it is the `app` whose page the caller acted on,
/// and its invocation is the one signed.
///
/// # Errors
///
/// The first check that fails, as [`InvocationError`].
pub fn verify_stamp(
    stamp: &collab_types::Stamp,
    workspace: &str,
    app: &str,
    revocations: &RevocationList,
) -> Result<Option<DeviceCert>, InvocationError> {
    let Some(call) = stamp.signed.as_deref() else {
        return Ok(None);
    };
    let signed = &call.signed;
    check_signature(signed)?;
    let statement = signed.statement();
    if statement.workspace != workspace {
        return Err(StatementMismatch::Workspace.into());
    }
    if stamp.invocation != statement.invocation.0.to_string() {
        return Err(StatementMismatch::Invocation.into());
    }
    // The page's call names the app whose page it is: the change must be that app's.
    if statement.target.address.resource != app {
        return Err(StatementMismatch::Target.into());
    }
    if call.argument_signed() == Some(false) {
        return Err(StatementMismatch::Arguments.into());
    }
    let cert = signer_of(
        signed,
        workspace,
        revocations,
        (statement.issued_at_ns, None),
    )?;
    let person = hex::encode(cert.user_public_key);
    // The stamp names the signer as its invoker, or — for an agent acting for
    // them — as the person it acted for.
    let named = stamp
        .invoked_by()
        .and_then(|actor| {
            actor
                .member_id()
                .and_then(|member| member.person_id())
                .map(str::to_owned)
        })
        .or_else(|| stamp.acted_for.clone());
    if named.as_deref() != Some(person.as_str()) {
        return Err(InvocationError::NotInvoker);
    }
    Ok(Some(cert))
}

/// Check the call a page's reader signed for what they did on the page of `app` in `workspace`,
/// as the device serving the page does before the act runs, and return their certificate: the
/// signature, that it names this workspace and this app, that its arguments are the act `asked`
/// (`{id, target, action}`, as the page sent it), its freshness at `now_ns`, and the
/// certificate's chain, validity and revocation. Every holder of the change checks it again
/// ([`verify_stamp`]).
///
/// # Errors
///
/// The first check that fails, as [`InvocationError`].
pub fn verify_page_call(
    call: &tilo_actions::remote::SignedCall,
    workspace: &str,
    app: &str,
    asked: (u32, Option<&str>, &crate::page::PageAction),
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, InvocationError> {
    #[derive(serde::Deserialize)]
    struct Act {
        id: u32,
        target: Option<String>,
        action: crate::page::PageAction,
    }
    let signed = &call.signed;
    check_signature(signed)?;
    let statement = signed.statement();
    if statement.workspace != workspace {
        return Err(StatementMismatch::Workspace.into());
    }
    if statement.target.address.resource != app {
        return Err(StatementMismatch::Target.into());
    }
    let (id, target, action) = asked;
    let names_act = match &call.argument {
        Some(tilo_actions::remote::InvokeArgument::Json(object)) => serde_json::from_value::<Act>(
            serde_json::Value::Object(object.clone()),
        )
        .is_ok_and(|signed| {
            signed.id == id && signed.target.as_deref() == target && &signed.action == action
        }),
        _ => false,
    };
    if !names_act || call.argument_signed() != Some(true) {
        return Err(StatementMismatch::Arguments.into());
    }
    if !statement.fresh_at(now_ns) {
        return Err(InvocationError::Stale);
    }
    signer_of(signed, workspace, revocations, (now_ns, None))
}

/// Check `request` in `workspace` as [`verify_invocation`] does, for the agent `agent` acting
/// with its own key for the person `person`: the key that signed is one that person certified
/// for that agent, reaching this workspace, and the very key `agent_key` the caller was
/// admitted as.
///
/// # Errors
///
/// As [`verify_invocation`]; [`InvocationError::NotAgentSigned`] when a device's own key or a
/// key that acts as the person signed, [`InvocationError::NotInvoker`] when another person's
/// key signed, or a key certified for another agent or for no agent in particular.
pub fn verify_agent_invocation(
    workspace: &str,
    request: &InvokeRequest,
    (person, agent, agent_key): (&[u8; 32], &str, &[u8; 32]),
    revocations: &RevocationList,
    now_ns: u64,
) -> Result<DeviceCert, InvocationError> {
    let cert = verify_invocation(workspace, request, revocations, now_ns)?;
    if !signs_as_agent(&cert, agent) {
        return Err(InvocationError::NotAgentSigned);
    }
    if &cert.user_public_key != person
        || &cert.device_public_key != agent_key
        || !certified_for(&cert, agent)
    {
        return Err(InvocationError::NotInvoker);
    }
    Ok(cert)
}

/// Whether the agent's key `cert` was certified for the agent `agent` alone: a key's agent is
/// the key its id names, an agent device acts by the delegation naming it beside its
/// certificate, and any other agent's grant names it ([`tilo_user_identity::AgentScope::is_agent`]).
/// A grant naming only a workspace, or every one, is no single agent's.
fn certified_for(cert: &DeviceCert, agent: &str) -> bool {
    match tilo_action_policy::key_of_agent(agent) {
        Some(key) => key == cert.device_public_key,
        None => cert.capabilities.agent.map_or_else(
            || cert.capabilities == tilo_user_identity::DeviceCapabilities::nothing(),
            |scope| scope.is_agent(agent),
        ),
    }
}

/// Whether `cert` is a key its person certified for an agent: an agent's grant, or an agent
/// device's certificate, which carries no capability and acts by the delegation beside it.
/// Never a device's own key, nor a key the person made for their own programs: that one signs
/// only as the agent it is itself ([`signs_as_agent`]).
fn agent_grant(cert: &DeviceCert) -> bool {
    let capabilities = &cert.capabilities;
    (capabilities.agent.is_some() && !capabilities.server_key)
        || *capabilities == tilo_user_identity::DeviceCapabilities::nothing()
}

/// Whether `cert` signs as the agent `agent`: a key its person certified for an agent
/// ([`agent_grant`]), or a key the person made for their own programs acting as the agent the
/// key itself is ([`tilo_action_policy::key_agent_id`]), which is how every host admits such a
/// key. That key signs for no other agent, and a device's own key signs for none.
fn signs_as_agent(cert: &DeviceCert, agent: &str) -> bool {
    agent_grant(cert)
        || (cert.capabilities.agent.is_some()
            && tilo_action_policy::key_of_agent(agent) == Some(cert.device_public_key))
}

/// The agent `acting` names as the one who acted with its own key: an agent no device of its
/// person handed its session to. `None` for a person, and for an agent a person's own device
/// signs for ([`tilo_action_policy::Acting::handed`]).
fn own_key_agent(acting: &tilo_action_policy::Acting) -> Option<&str> {
    acting.agent_id().filter(|_| !acting.handed)
}

/// That the invoker a change names and the key that signed its invocation agree: an agent
/// acting with its own key is signed by a key of the person it acts for certified for that
/// agent ([`certified_for`]), and an agent's grant signs only for an agent, never as its
/// person: for itself, or for a sub-agent it started, which it hands its work as a person's
/// device hands an agent its session.
fn agent_signed(
    invoker: Option<&tilo_action_policy::Acting>,
    cert: &DeviceCert,
) -> Result<(), InvocationError> {
    let agent = invoker.and_then(own_key_agent);
    match (agent, agent_grant(cert)) {
        (Some(agent), _) if !signs_as_agent(cert, agent) => Err(InvocationError::NotAgentSigned),
        (None, true) if !invoker.is_some_and(|acting| acting.handed && acting.is_agent()) => {
            Err(InvocationError::NotInvoker)
        }
        (Some(agent), true) if !certified_for(cert, agent) => Err(InvocationError::NotInvoker),
        _ => Ok(()),
    }
}

/// That `signed`'s signature verifies under the key it names.
fn check_signature(signed: &SignedInvocation) -> Result<(), InvocationError> {
    let statement = signed.statement();
    let key = ed25519_dalek::VerifyingKey::from_bytes(&statement.signer)
        .map_err(|_| InvocationError::Signature)?;
    key.verify_strict(
        &statement.signing_message(),
        &ed25519_dalek::Signature::from_bytes(signed.signature()),
    )
    .map_err(|_| InvocationError::Signature)
}

/// The certificate `signed` carries, checked: it certifies the signing key,
/// chains to its person's root and is valid at `at_ns`, is not revoked in
/// `revocations` (counted from `revoked_from`, when given, else whenever it
/// was), and a grant reaches `workspace`.
fn signer_of(
    signed: &SignedInvocation,
    workspace: &str,
    revocations: &RevocationList,
    (at_ns, revoked_from): (u64, Option<u64>),
) -> Result<DeviceCert, InvocationError> {
    let cert = decode_cert(signed.signer_cert())?;
    if cert.device_public_key != signed.statement().signer {
        return Err(InvocationError::SignerMismatch);
    }
    verify_device_cert_at(&cert, at_ns).map_err(InvocationError::Certificate)?;
    let revoked = match revoked_from {
        Some(from) => revocations.refuses_at(&cert, from),
        None => revocations.refuses(&cert),
    };
    if revoked {
        return Err(InvocationError::Revoked);
    }
    if cert
        .capabilities
        .agent
        .is_some_and(|scope| !scope.reaches(workspace))
    {
        return Err(InvocationError::OutOfScope);
    }
    Ok(cert)
}

fn decode_cert(bytes: &[u8]) -> Result<DeviceCert, InvocationError> {
    if bytes.len() > MAX_SIGNER_CERT_BYTES {
        return Err(InvocationError::Encoding);
    }
    match postcard::take_from_bytes::<DeviceCert>(bytes) {
        Ok((cert, [])) => Ok(cert),
        _ => Err(InvocationError::Encoding),
    }
}

/// The invocation ids a host admitted within the freshness window, so one id
/// names one request: the same id signed again for the same request by the
/// same key is a retry, which the host's invocation log answers with what the
/// first did, and the same id for anything else is a conflict.
#[derive(Debug, Default)]
pub struct InvocationReplay {
    seen: std::collections::HashMap<tilo_actions::InvocationId, Seen>,
}

#[derive(Debug)]
struct Seen {
    /// The request as signed, without its issue time.
    request: [u8; 32],
    /// When the last statement for it was issued.
    issued_at_ns: u64,
}

/// At most this many invocations are remembered; past it, the oldest go
/// first. The freshness window bounds what a forgotten one could replay.
const MAX_REMEMBERED: usize = 65_536;

impl InvocationReplay {
    /// Admit the verified `signed` at `now_ns`.
    ///
    /// # Errors
    ///
    /// [`InvocationError::Replayed`] when its id was admitted within the
    /// window for another request or another signer.
    pub fn admit(&mut self, signed: &SignedInvocation, now_ns: u64) -> Result<(), InvocationError> {
        let statement = signed.statement();
        let floor = now_ns.saturating_sub(INVOCATION_SIGNATURE_TOLERANCE_NS);
        if self.seen.len() >= MAX_REMEMBERED {
            self.seen.retain(|_, seen| seen.issued_at_ns >= floor);
            if self.seen.len() >= MAX_REMEMBERED
                && let Some(oldest) = self
                    .seen
                    .iter()
                    .min_by_key(|(_, seen)| seen.issued_at_ns)
                    .map(|(id, _)| *id)
            {
                self.seen.remove(&oldest);
            }
        }
        let request = request_digest(statement);
        match self.seen.get_mut(&statement.invocation) {
            Some(seen) if seen.issued_at_ns >= floor && seen.request != request => {
                Err(InvocationError::Replayed)
            }
            Some(seen) => {
                seen.request = request;
                seen.issued_at_ns = seen.issued_at_ns.max(statement.issued_at_ns);
                Ok(())
            }
            None => {
                self.seen.insert(
                    statement.invocation,
                    Seen {
                        request,
                        issued_at_ns: statement.issued_at_ns,
                    },
                );
                Ok(())
            }
        }
    }
}

/// What a statement asks, whenever it was issued.
fn request_digest(statement: &InvocationStatement) -> [u8; 32] {
    let timeless = InvocationStatement {
        issued_at_ns: 0,
        ..statement.clone()
    };
    *blake3::hash(&timeless.signing_message()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_action_policy::Acting;
    use tilo_actions::ActionAddress;
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};
    use tilo_user_identity::{
        AgentScope, DeviceCapabilities, RevocationEntry, RevocationReason, UserIdentity,
        prepare_minted_grant, sign_device_cert, sign_revocation_entry,
    };

    const WORKSPACE: &str = "6f1d2c3b-4a59-4e7f-8a1b-2c3d4e5f6a7b";
    const NOW: u64 = 10_000_000_000_000;

    fn cert_of(root: &SigningKey, key: &SigningKey, expires_at_ns: Option<u64>) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// `key` certified by `root` as an agent's grant reaching `scope`, as a person's device
    /// certifies the key it gives an agent.
    fn agent_grant_of(root: &SigningKey, key: &SigningKey, scope: AgentScope) -> DeviceCert {
        let mut cert = cert_of(root, key, Some(NOW + 1_000_000_000));
        cert.capabilities = DeviceCapabilities::agent_grant(scope);
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn request() -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: "so.tilo.notes".into(),
                address: ActionAddress::new("note:1", "rename"),
            },
            argument: InvokeArgument::Text("Plan".into()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn signed(
        request: &InvokeRequest,
        cert: &DeviceCert,
        key: &SigningKey,
        at: u64,
    ) -> InvokeRequest {
        let mut request = request.clone();
        request.signed = Some(Box::new(
            sign_invocation_now(WORKSPACE, &request, cert, key, at).unwrap(),
        ));
        request
    }

    tilo_test_env::scenario! {
        fn a_host_takes_an_invocation_only_as_its_signer_signed_it;

        phase a_signed_invocation_verifies_as_its_signer;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let asked = signed(&request(), &cert, &key, NOW);
            let verified = verify_invocation(WORKSPACE, &asked, &RevocationList::new(), NOW).unwrap();
            assert_eq!(verified, cert);
        }

        phase a_changed_workspace_target_or_argument_is_refused;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let asked = signed(&request(), &cert, &key, NOW);
            let none = RevocationList::new();
            assert_eq!(
                verify_invocation("another", &asked, &none, NOW),
                Err(InvocationError::Mismatch(StatementMismatch::Workspace))
            );
            let mut retargeted = asked.clone();
            retargeted.target.address.action = "delete".into();
            assert_eq!(
                verify_invocation(WORKSPACE, &retargeted, &none, NOW),
                Err(InvocationError::Mismatch(StatementMismatch::Target))
            );
            let mut reargued = asked;
            reargued.argument = InvokeArgument::Text("Other".into());
            assert_eq!(
                verify_invocation(WORKSPACE, &reargued, &none, NOW),
                Err(InvocationError::Mismatch(StatementMismatch::Arguments))
            );
            // A statement rewritten to match is no longer the one signed.
            let mut forged = reargued;
            let original = forged.signed.take().unwrap();
            let rewritten = InvocationStatement::of(WORKSPACE, &forged, NOW, original.statement().signer);
            forged.signed = Some(Box::new(SignedInvocation::new(
                rewritten,
                *original.signature(),
                original.signer_cert().to_vec(),
            )));
            assert_eq!(
                verify_invocation(WORKSPACE, &forged, &none, NOW),
                Err(InvocationError::Signature)
            );
            assert_eq!(
                verify_invocation(WORKSPACE, &request(), &none, NOW),
                Err(InvocationError::Unsigned)
            );
        }

        phase a_stale_expired_or_revoked_signer_is_refused;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let none = RevocationList::new();
            let cert = cert_of(&root, &key, None);
            let old = signed(&request(), &cert, &key, NOW - INVOCATION_SIGNATURE_TOLERANCE_NS - 1);
            assert_eq!(verify_invocation(WORKSPACE, &old, &none, NOW), Err(InvocationError::Stale));
            let expiring = cert_of(&root, &key, Some(NOW - 1));
            let expired = signed(&request(), &expiring, &key, NOW);
            assert_eq!(
                verify_invocation(WORKSPACE, &expired, &none, NOW),
                Err(InvocationError::Certificate(DeviceCertError::Expired))
            );
            let mut entry = RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: cert.device_public_key,
                revoked_at_ns: 500,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &root).unwrap();
            let mut revoked = RevocationList::new();
            revoked.insert(entry).unwrap();
            let asked = signed(&request(), &cert, &key, NOW);
            assert_eq!(
                verify_invocation(WORKSPACE, &asked, &revoked, NOW),
                Err(InvocationError::Revoked)
            );
            // Another key cannot sign under this certificate.
            let other = SigningKey::from_bytes(&[8; 32]);
            assert_eq!(
                sign_invocation_now(WORKSPACE, &request(), &cert, &other, NOW),
                Err(InvocationError::SignerMismatch)
            );
        }

        phase a_minted_key_carries_its_issuer_and_reaches_only_its_workspace;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let account = SigningKey::from_bytes(&[9; 32]);
            let mut issuer = cert_of(&root, &account, Some(NOW + 1_000_000_000));
            issuer.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
            sign_device_cert(&mut issuer, &root).unwrap();
            let minted_key = SigningKey::from_bytes(&[10; 32]);
            let workspace = *uuid::Uuid::parse_str(WORKSPACE).unwrap().as_bytes();
            let mut minted = prepare_minted_grant(
                &issuer,
                minted_key.verifying_key().to_bytes(),
                workspace,
                "script",
                2,
                NOW + 1_000_000_000,
            );
            sign_device_cert(&mut minted, &account).unwrap();
            let asked = signed(&request(), &minted, &minted_key, NOW);
            let none = RevocationList::new();
            let verified = verify_invocation(WORKSPACE, &asked, &none, NOW).unwrap();
            assert_eq!(verified.issuer.as_deref(), Some(&issuer));
            let mut elsewhere = request();
            let other = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
            elsewhere.signed = Some(Box::new(
                sign_invocation_now(other, &elsewhere, &minted, &minted_key, NOW).unwrap(),
            ));
            assert_eq!(
                verify_invocation(other, &elsewhere, &none, NOW),
                Err(InvocationError::OutOfScope)
            );
        }

        phase an_agent_asks_only_under_the_key_its_person_certified_for_it;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let person = root.verifying_key().to_bytes();
            let workspace = *uuid::Uuid::parse_str(WORKSPACE).unwrap().as_bytes();
            let key = |seed| SigningKey::from_bytes(&[seed; 32]);
            let grant = |key: &SigningKey, scope| agent_grant_of(&root, key, scope);
            let mut own = cert_of(&root, &key(13), None);
            own.capabilities = DeviceCapabilities::own_key_grant(AgentScope::Workspace(workspace));
            sign_device_cert(&mut own, &root).unwrap();
            let mut entry = RevocationEntry {
                user_public_key: person,
                revoked_device_public_key: key(11).verifying_key().to_bytes(),
                revoked_at_ns: NOW - 1,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, &root).unwrap();
            let mut revoked = RevocationList::new();
            revoked.insert(entry).unwrap();
            let none = RevocationList::new();
            let agent = key(11).verifying_key().to_bytes();
            let scribe = |workspace| AgentScope::agent_in(workspace, "scribe");
            let ours = grant(&key(11), scribe(workspace));
            for (cert, by, revocations, expected) in [
                (ours.clone(), key(11), &none, Ok(ours.clone())),
                // A key the person made to act as themselves never acts as their agent.
                (own, key(13), &none, Err(InvocationError::NotAgentSigned)),
                // Another of their agents' keys is not this agent's, nor is a key certified for
                // another agent, or for no agent in particular, though the very key signs.
                (grant(&key(14), scribe(workspace)), key(14), &none, Err(InvocationError::NotInvoker)),
                (grant(&key(11), AgentScope::agent_in(workspace, "critic")), key(11), &none, Err(InvocationError::NotInvoker)),
                (grant(&key(11), AgentScope::Workspace(workspace)), key(11), &none, Err(InvocationError::NotInvoker)),
                (grant(&key(11), scribe([7; 16])), key(11), &none, Err(InvocationError::OutOfScope)),
                (ours, key(11), &revoked, Err(InvocationError::Revoked)),
            ] {
                let asked = signed(&request(), &cert, &by, NOW);
                assert_eq!(
                    verify_agent_invocation(WORKSPACE, &asked, (&person, "scribe", &agent), revocations, NOW),
                    expected
                );
            }
            // A key the person made for their programs acts as the agent it is itself, and as
            // no other.
            let mut server_key = cert_of(&root, &key(15), None);
            server_key.capabilities = DeviceCapabilities::server_key_grant();
            sign_device_cert(&mut server_key, &root).unwrap();
            let keyed = key(15).verifying_key().to_bytes();
            let asked = signed(&request(), &server_key, &key(15), NOW);
            let as_agent = |agent: &str| {
                verify_agent_invocation(WORKSPACE, &asked, (&person, agent, &keyed), &none, NOW)
            };
            assert_eq!(as_agent(&tilo_action_policy::key_agent_id(&keyed)), Ok(server_key.clone()));
            assert_eq!(as_agent("scribe"), Err(InvocationError::NotAgentSigned));
            assert_eq!(
                as_agent(&tilo_action_policy::key_agent_id(&[9; 32])),
                Err(InvocationError::NotAgentSigned)
            );
        }

        phase a_change_names_an_agent_only_when_its_own_key_asked;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let person = hex::encode(root.verifying_key().to_bytes());
            let workspace = *uuid::Uuid::parse_str(WORKSPACE).unwrap().as_bytes();
            let agent_key = SigningKey::from_bytes(&[11; 32]);
            let agent = agent_grant_of(&root, &agent_key, AgentScope::agent_in(workspace, "scout"));
            let laptop = SigningKey::from_bytes(&[12; 32]);
            let device = cert_of(&root, &laptop, None);
            let by_agent = signed(&request(), &agent, &agent_key, NOW);
            let by_device = signed(&request(), &device, &laptop, NOW);
            let for_person = |acting: Acting| acting.acting_for(Acting::person(person.clone()));
            let kept = |asked: &InvokeRequest, invoker: &Acting, signed: bool| {
                verify_recorded(
                    &tilo_actions::ActionContext {
                        source_invocation: Some(asked.invocation),
                        signed: signed.then(|| tilo_actions::remote::SignedCall::of(asked).map(Box::new)).flatten(),
                        invoker: Some(tilo_actions::ActionInvoker::admitted(invoker)),
                        ..tilo_actions::ActionContext::default()
                    },
                    (WORKSPACE, None),
                    &RevocationList::new(),
                    NOW,
                )
            };
            let scout = for_person(Acting::agent("scout"));
            assert_eq!(kept(&by_agent, &scout, true), Ok(Some(agent.clone())));
            // The agent's key never stands for its person, nor for a key's agent it is not.
            let keyed = for_person(Acting::agent(tilo_action_policy::key_agent_id(&[9; 32])));
            let critic = for_person(Acting::agent("critic"));
            for invoker in [Acting::person(person.clone()), keyed, critic] {
                assert_eq!(kept(&by_agent, &invoker, true), Err(InvocationError::NotInvoker));
            }
            // An agent only its person's device signed for, or that kept no signed call, is refused.
            assert_eq!(kept(&by_device, &scout, true), Err(InvocationError::NotAgentSigned));
            assert_eq!(kept(&by_agent, &scout, false), Err(InvocationError::NotAgentSigned));
            // An agent the person's own device handed its session to is that device's to sign.
            let handed = Acting::handed_agent("scout", person.as_str());
            assert_eq!(kept(&by_device, &handed, true), Ok(Some(device)));
            // A sub-agent the agent started works under the agent's key.
            let helper = Acting::handed_agent("helper", person.as_str());
            assert_eq!(kept(&by_agent, &helper, true), Ok(Some(agent)));
            // A key the person made for their programs is kept as the agent it is itself, and its
            // signature names no other agent, nor the agent the key's id names unsigned.
            let program = SigningKey::from_bytes(&[13; 32]);
            let mut server_key = cert_of(&root, &program, None);
            server_key.capabilities = DeviceCapabilities::server_key_grant();
            sign_device_cert(&mut server_key, &root).unwrap();
            let by_key = signed(&request(), &server_key, &program, NOW);
            let itself = for_person(Acting::agent(tilo_action_policy::key_agent_id(
                &program.verifying_key().to_bytes(),
            )));
            assert_eq!(kept(&by_key, &itself, true), Ok(Some(server_key)));
            assert_eq!(kept(&by_key, &scout, true), Err(InvocationError::NotAgentSigned));
            assert_eq!(kept(&by_key, &itself, false), Err(InvocationError::NotAgentSigned));
        }

        phase a_change_outside_the_calls_target_is_refused;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let asked = signed(&request(), &cert, &key, NOW);
            let acting = Acting::person(hex::encode(cert.user_public_key));
            let change = |id: &str, kind: &str, action: &str| {
                let mut context = tilo_actions::ActionContext {
                    source_invocation: Some(asked.invocation),
                    signed: tilo_actions::remote::SignedCall::of(&asked).map(Box::new),
                    invoker: Some(tilo_actions::ActionInvoker::admitted(&acting)),
                    resource: Some(tilo_actions::ActionResource {
                        id: id.into(),
                        kind: kind.into(),
                        label: String::new(),
                    }),
                    ..tilo_actions::ActionContext::default()
                };
                context.what = Some(tilo_actions::Did::Verb(tilo_actions::VerbCall {
                    action: tilo_actions::ActionKey {
                        app: "so.tilo.notes".into(),
                        kind: "note".into(),
                        action: action.into(),
                    },
                    args: std::collections::BTreeMap::new(),
                }));
                context
            };
            let none = RevocationList::new();
            // The call's own resource and action are inside it; another of either is not.
            assert_eq!(
                verify_recorded(&change("note:1", "note", "rename"), (WORKSPACE, None), &none, NOW),
                Ok(Some(cert))
            );
            for outside in [
                change("note:2", "note", "rename"),
                change("note:1", "note", "delete"),
            ] {
                assert_eq!(
                    verify_recorded(&outside, (WORKSPACE, None), &none, NOW),
                    Err(InvocationError::Mismatch(StatementMismatch::Target))
                );
            }
        }

        phase a_step_the_called_action_declares_counts_only_from_its_records_keeper;
        // Ada keeps an item in the workspace's vault: her call names the vault's `add`, and the
        // host that keeps the vault's record writes the step `add` declares there. The same step
        // from a device that does not keep the record, and a step `add` does not declare, are
        // refused.
        {
            use tilo_actions::remote::person_vault;
            let (root, _) = UserIdentity::generate("Ada", 1);
            let key = SigningKey::from_bytes(&[31; 32]);
            let cert = cert_of(&root, &key, None);
            let mut adding = request();
            adding.target = person_vault::target(person_vault::ADD);
            let asked = signed(&adding, &cert, &key, NOW);
            let acting = Acting::person(hex::encode(cert.user_public_key));
            let passwords = tilo_action_policy::roles::protected_resource_kind(
                "so.tilo.passwords",
                tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
            )
            .expect("the vault's record has its kind");
            let step = |verb: &str| {
                let mut context = tilo_actions::ActionContext {
                    source_invocation: Some(asked.invocation),
                    signed: tilo_actions::remote::SignedCall::of(&asked).map(Box::new),
                    invoker: Some(tilo_actions::ActionInvoker::admitted(&acting)),
                    resource: Some(tilo_actions::ActionResource {
                        id: "vault-record".into(),
                        kind: passwords.into(),
                        label: String::new(),
                    }),
                    ..tilo_actions::ActionContext::default()
                };
                context.what = Some(tilo_actions::Did::Verb(tilo_actions::VerbCall {
                    action: tilo_actions::ActionKey {
                        app: String::new(),
                        kind: passwords.into(),
                        action: verb.into(),
                    },
                    args: std::collections::BTreeMap::new(),
                }));
                context
            };
            let none = RevocationList::new();
            let kept_by = |keeper| verify_recorded(&step("keep-sealed"), (WORKSPACE, keeper), &none, NOW);
            assert_eq!(kept_by(Some("so.tilo.passwords")), Ok(Some(cert)));
            let refused = Err(InvocationError::Mismatch(StatementMismatch::Target));
            assert_eq!(kept_by(None), refused);
            assert_eq!(kept_by(Some("so.tilo.notes")), refused);
            assert_eq!(
                verify_recorded(&step("remove-sealed"), (WORKSPACE, Some("so.tilo.passwords")), &none, NOW),
                refused
            );
        }

        phase a_device_writes_only_for_its_own_person_and_the_agents_it_runs;
        // Ada's agent answers Bo, and Ada's device signs: the call checks, as
        // Bo and Ada are both named, but the member it names is Bo's and her
        // device may not write for him. Only Bo's own call carries it.
        {
            use tilo_action_policy::{
                ActionKind, Answer, Context, MemberAccess, Role, TreePath, WorkspacePolicy,
            };
            let (ada_root, _) = UserIdentity::generate("Ada", 1);
            let (bo_root, _) = UserIdentity::generate("Bo", 1);
            let (eve_root, _) = UserIdentity::generate("Eve", 1);
            let ada = hex::encode(ada_root.verifying_key().to_bytes());
            let bo = hex::encode(bo_root.verifying_key().to_bytes());
            let laptop = SigningKey::from_bytes(&[21; 32]);
            let ada_device = cert_of(&ada_root, &laptop, None);
            let eve_device = cert_of(&eve_root, &SigningKey::from_bytes(&[22; 32]), None);
            let answering_bo = Acting::handed_agent("scout", bo.as_str()).agent_of(ada.as_str());
            let kept = |asked: &InvokeRequest| {
                verify_recorded(
                    &tilo_actions::ActionContext {
                        source_invocation: Some(asked.invocation),
                        signed: tilo_actions::remote::SignedCall::of(asked).map(Box::new),
                        invoker: Some(tilo_actions::ActionInvoker::admitted(&answering_bo)),
                        ..tilo_actions::ActionContext::default()
                    },
                    (WORKSPACE, None),
                    &RevocationList::new(),
                    NOW,
                )
            };
            let by_ada = signed(&request(), &ada_device, &laptop, NOW);
            assert_eq!(kept(&by_ada), Ok(Some(ada_device)));
            // A person the chain does not name signs nothing in it.
            let by_eve = signed(&request(), &eve_device, &SigningKey::from_bytes(&[22; 32]), NOW);
            assert_eq!(kept(&by_eve), Err(InvocationError::NotInvoker));

            let mut policy = WorkspacePolicy::new(WORKSPACE, ada.as_str());
            policy.members.insert(bo, Role::Editor);
            let policy = policy.with_current_roles();
            let notes = TreePath::workspace(WORKSPACE).app("notes").action("append");
            let cx = Context::at(NOW);
            let signer = MemberAccess::person(policy, ada.as_str());
            assert!(
                signer.invoked_by(&answering_bo).is_none(),
                "Ada's device writes for no member Bo named"
            );
            let for_ada = Acting::handed_agent("scout", ada.as_str());
            let change = signer
                .invoked_by(&for_ada)
                .expect("Ada's device runs the agent she hands its session");
            assert_eq!(change.answer(&notes, ActionKind::Undoable, &cx), Answer::Allow);
        }

        phase one_invocation_id_names_one_request;
        {
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let first = signed(&request(), &cert, &key, NOW);
            let mut replay = InvocationReplay::default();
            replay.admit(first.signed.as_ref().unwrap(), NOW).unwrap();
            // Signed again for the same request: a retry.
            let retry = signed(&first, &cert, &key, NOW + 1);
            replay.admit(retry.signed.as_ref().unwrap(), NOW + 1).unwrap();
            // The same id for another action.
            let mut other = first;
            other.target.address.action = "delete".into();
            let other = signed(&other, &cert, &key, NOW + 2);
            assert_eq!(
                replay.admit(other.signed.as_ref().unwrap(), NOW + 2),
                Err(InvocationError::Replayed)
            );
        }

        phase a_page_call_checks_only_for_the_act_it_names;
        {
            use crate::page::PageAction;
            let (root, _) = UserIdentity::generate("Alice", 1);
            let key = SigningKey::from_bytes(&[7; 32]);
            let cert = cert_of(&root, &key, None);
            let act = serde_json::json!({"id": 7, "target": "notes.block-text:b1", "action": {"value": "Milk"}});
            let mut asked = request();
            asked.target = RemoteActionTarget {
                provider: "notes".into(),
                address: ActionAddress::new("notes", "act"),
            };
            asked.argument = InvokeArgument::Json(act.as_object().unwrap().clone());
            let call = tilo_actions::remote::SignedCall::of(&signed(&asked, &cert, &key, NOW)).unwrap();
            let none = RevocationList::new();
            let milk = PageAction::Value("Milk".into());
            let check = |call: &tilo_actions::remote::SignedCall, app: &str, act: (u32, Option<&str>, &PageAction)| {
                verify_page_call(call, WORKSPACE, app, act, &none, NOW)
            };
            assert_eq!(check(&call, "notes", (7, Some("notes.block-text:b1"), &milk)), Ok(cert));
            // Another element, other text, or another app's page: not what was signed.
            let mismatch = Err(InvocationError::Mismatch(StatementMismatch::Arguments));
            assert_eq!(check(&call, "notes", (8, Some("notes.block-text:b1"), &milk)), mismatch);
            let eggs = PageAction::Value("Eggs".into());
            assert_eq!(check(&call, "notes", (7, Some("notes.block-text:b1"), &eggs)), mismatch);
            assert_eq!(
                check(&call, "mail", (7, Some("notes.block-text:b1"), &milk)),
                Err(InvocationError::Mismatch(StatementMismatch::Target))
            );
            // The argument swapped under the signature.
            let mut swapped = call;
            swapped.argument = Some(InvokeArgument::Json(
                serde_json::json!({"id": 7, "target": "notes.block-text:b1", "action": {"value": "Eggs"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ));
            assert_eq!(check(&swapped, "notes", (7, Some("notes.block-text:b1"), &eggs)), mismatch);
        }
    }
}
