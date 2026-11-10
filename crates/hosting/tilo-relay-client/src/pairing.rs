//! What a person's root gives their devices, and the pairing ceremony their
//! host carries between them: a new device asks to be certified
//! ([`certify`](crate::certify)), a root-holding device lists the waiting
//! requests ([`pending_pairings`]) and answers one ([`answer_pairing`]).
//!
//! Only the root signs: the host carries and checks, and hands nothing on
//! that a root did not sign for exactly that request.

use tilo_relay_protocol::certify::{
    ApproverDecision, ApproverReply, ApproverRequest, MemberApproval,
};
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, MAX_AGENT_GRANT_LIFETIME_NS,
    RevocationEntry, RevocationReason, STAND_IN_LIFETIME_NS,
    certification::MAX_CERTIFICATION_LABEL_BYTES, verify_device_cert_at,
};

use crate::error::CertifyError;
use crate::https::{self, HttpClient};
use crate::signer::DeviceSigner;

/// How long a node's certificate lasts.
pub const NODE_CERT_LIFETIME_NS: u64 = 365 * 24 * 60 * 60 * 1_000_000_000;

/// What a certificate the person's root issues lets its device do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuedRole {
    /// A browser: [`CertificationPolicy::browser`] (a member, 30 days).
    Browser,
    /// A node that hosts and governs the person's workspaces, admitting no
    /// devices. One year.
    Node,
    /// The session beside a node, running the person's apps and agents: a
    /// member device of theirs with its own key, never a host. One year.
    Session,
}

impl IssuedRole {
    /// The capabilities granted.
    #[must_use]
    pub fn capabilities(self) -> DeviceCapabilities {
        match self {
            Self::Browser => CertificationPolicy::browser().ceiling,
            Self::Node => tilo_user_identity::cloud_enrollment::host_capabilities(),
            Self::Session => DeviceCapabilities::member(),
        }
    }

    fn lifetime_ns(self) -> u64 {
        match self {
            Self::Browser => CertificationPolicy::browser().lifetime_ns,
            Self::Node | Self::Session => NODE_CERT_LIFETIME_NS,
        }
    }
}

/// Have `root` (the person's user root) certify `device_public_key` as
/// `role`, issued now; never the root itself.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, or when signing fails or
/// does not verify.
pub async fn issue_device_cert(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    label: &str,
    role: IssuedRole,
) -> Result<DeviceCert, CertifyError> {
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "the root certifies devices, never itself".into(),
        ));
    }
    let now = tilo_clock::unix_now_ns();
    let cert = DeviceCert {
        user_public_key,
        device_public_key,
        device_label: label.to_owned(),
        capabilities: role.capabilities(),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(role.lifetime_ns())),
        signature: [0; 64],
        issuer: None,
    };
    let cert = signed(root, cert).await?;
    verify_device_cert_at(&cert, now).map_err(|error| CertifyError::Request(error.to_string()))?;
    Ok(cert)
}

/// `cert` as `root` signs it, its digest checked first.
async fn signed(root: &dyn DeviceSigner, mut cert: DeviceCert) -> Result<DeviceCert, CertifyError> {
    let digest = tilo_user_identity::device_cert_signing_digest(&cert)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    cert.signature = root
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    Ok(cert)
}

/// Have `root` certify `device_public_key` as an agent's grant with
/// `capabilities`, expiring at `expires_at_ns`: a service runs the agent for
/// the person in exactly those workspaces, until then. Only a server key may
/// last until revoked (`None`); a grant admits no devices and has no life of
/// its own to extend.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, capabilities that are no
/// grant's, an expiry not in the future or (but for a server key) missing or
/// beyond [`MAX_AGENT_GRANT_LIFETIME_NS`], a label a certificate does not
/// carry, or a signature that does not verify.
pub async fn issue_agent_grant(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    label: &str,
    capabilities: DeviceCapabilities,
    expires_at_ns: Option<u64>,
) -> Result<DeviceCert, CertifyError> {
    let request = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "the root certifies devices, never itself".into(),
        ));
    }
    let label = label.trim();
    if label.is_empty()
        || label.len() > MAX_CERTIFICATION_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(CertifyError::Request(
            "a grant's label names who holds it".into(),
        ));
    }
    let now = tilo_clock::unix_now_ns();
    if capabilities.agent.is_none() {
        return Err(CertifyError::Request("a grant reaches workspaces".into()));
    }
    let bounded = expires_at_ns.is_some_and(|expiry| {
        expiry > now && expiry.saturating_sub(now) <= MAX_AGENT_GRANT_LIFETIME_NS
    });
    let chosen = capabilities.server_key && expires_at_ns.is_none_or(|expiry| expiry > now);
    if !(bounded || chosen) {
        return Err(CertifyError::Request(
            "a grant expires after now, and within 90 days unless it is a server key".into(),
        ));
    }
    let cert = DeviceCert {
        user_public_key,
        device_public_key,
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    let cert = signed(root, cert).await?;
    verify_device_cert_at(&cert, now).map_err(|e| request(&e))?;
    Ok(cert)
}

/// Have `root` make `stand_in_public_key`, a key the person's host holds, the
/// person's stand-in approver for `lifetime_ns`: while the root lives in one
/// tab, it certifies a new member browser of the person a device of theirs
/// asked for, and nothing else. The root revokes it once they keep their root
/// with a passkey; what it certified before stays valid.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, a lifetime of zero or
/// beyond [`STAND_IN_LIFETIME_NS`], a label a certificate does not carry, or a
/// signature that does not verify.
pub async fn issue_stand_in(
    root: &dyn DeviceSigner,
    stand_in_public_key: [u8; 32],
    label: &str,
    lifetime_ns: u64,
) -> Result<DeviceCert, CertifyError> {
    let user_public_key = root.device_public_key();
    if stand_in_public_key == user_public_key {
        return Err(CertifyError::Request(
            "the root certifies a stand-in, never itself".into(),
        ));
    }
    let label = label.trim();
    if label.is_empty()
        || label.len() > MAX_CERTIFICATION_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(CertifyError::Request(
            "a stand-in's label names who holds it".into(),
        ));
    }
    if lifetime_ns == 0 || lifetime_ns > STAND_IN_LIFETIME_NS {
        return Err(CertifyError::Request(
            "a stand-in lasts for a while, and at most 30 days".into(),
        ));
    }
    let now = tilo_clock::unix_now_ns();
    let cert = tilo_user_identity::prepare_stand_in_cert(
        user_public_key,
        stand_in_public_key,
        label,
        now,
        now.saturating_add(lifetime_ns),
    );
    let cert = signed(root, cert).await?;
    tilo_user_identity::verify_stand_in_cert_at(&cert, now)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    Ok(cert)
}

/// Have `root` revoke `device_public_key` as of `at_ns`, for `reason`: the
/// entry every host and device refuses that key from then on with, while what
/// it signed before stays valid. A stand-in approver is revoked this way once
/// the person keeps their root with a passkey.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, when signing fails, or
/// when the entry does not verify.
pub async fn revoke_device(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    at_ns: u64,
    reason: RevocationReason,
) -> Result<RevocationEntry, CertifyError> {
    let request = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "a root revokes the keys it made, never itself".into(),
        ));
    }
    let mut entry = RevocationEntry {
        user_public_key,
        revoked_device_public_key: device_public_key,
        revoked_at_ns: at_ns,
        reason,
        signature: [0; 64],
    };
    let digest = tilo_user_identity::revocation_signing_digest(&entry).map_err(|e| request(&e))?;
    entry.signature = root
        .sign(digest.to_vec())
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    tilo_user_identity::verify_revocation_entry(&entry).map_err(|e| request(&e))?;
    Ok(entry)
}

async fn member_approval(
    root: &dyn DeviceSigner,
    request: ApproverRequest,
) -> Result<MemberApproval, CertifyError> {
    let user_public_key = root.device_public_key();
    let issued_at_ns = tilo_clock::unix_now_ns();
    let message = MemberApproval::signed_message(&user_public_key, issued_at_ns, &request)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let signature = root
        .sign(message)
        .await
        .map_err(|error| CertifyError::Request(error.0))?;
    Ok(MemberApproval {
        user_public_key,
        issued_at_ns,
        request,
        signature,
    })
}

async fn approve(
    client: &dyn HttpClient,
    root: &dyn DeviceSigner,
    request: ApproverRequest,
) -> Result<ApproverReply, CertifyError> {
    let approval = member_approval(root, request).await?;
    match https::certify_approve(client, &approval).await? {
        ApproverReply::Refused { reason } => Err(CertifyError::Denied(reason)),
        reply => Ok(reply),
    }
}

/// The requests of new devices waiting at the host at `client` for `root` (a
/// member's root) to answer, each showing its pairing code.
///
/// # Errors
///
/// [`CertifyError::Transport`] when the host is not reached, or
/// [`CertifyError::Denied`] when it refuses.
pub async fn pending_pairings(
    client: &dyn HttpClient,
    root: &dyn DeviceSigner,
) -> Result<Vec<tilo_relay_protocol::certify::PendingCertification>, CertifyError> {
    match approve(client, root, ApproverRequest::ListPending).await? {
        ApproverReply::Pending(pending) => Ok(pending),
        _ => Err(CertifyError::Denied(
            "the host answered something else".into(),
        )),
    }
}

/// Answer the pending request `request_digest` at the host at `client`:
/// approved, `root` certifies the device it names as a browser of that root,
/// and the certificate it issued is answered for the root's own record.
///
/// # Errors
///
/// [`CertifyError::Denied`] when the request is gone or the host refuses, and
/// [`CertifyError::Transport`] when it is not reached.
pub async fn answer_pairing(
    client: &dyn HttpClient,
    root: &dyn DeviceSigner,
    request_digest: [u8; 32],
    approved: bool,
) -> Result<Option<DeviceCert>, CertifyError> {
    let decision = if approved {
        let pending = pending_pairings(client, root).await?;
        let Some(found) = pending
            .iter()
            .find(|entry| entry.request_digest == request_digest)
        else {
            return Err(CertifyError::Denied("that device no longer waits".into()));
        };
        let request = &found.request;
        ApproverDecision::Issue(
            issue_device_cert(
                root,
                request.device_public_key,
                &request.device_label,
                IssuedRole::Browser,
            )
            .await?,
        )
    } else {
        ApproverDecision::Deny
    };
    let issued = match &decision {
        ApproverDecision::Issue(cert) => Some(cert.clone()),
        _ => None,
    };
    decide_pairing(client, root, request_digest, issued.clone())
        .await
        .map(|()| issued)
}

/// Answer the pending request `request_digest` at the host at `client`, as
/// `approver`: with `cert`, one the person's root already issued for exactly
/// that request, or turned away without one.
///
/// # Errors
///
/// As [`answer_pairing`].
pub async fn decide_pairing(
    client: &dyn HttpClient,
    approver: &dyn DeviceSigner,
    request_digest: [u8; 32],
    cert: Option<DeviceCert>,
) -> Result<(), CertifyError> {
    let decision = match cert {
        Some(cert) => ApproverDecision::Issue(cert),
        None => ApproverDecision::Deny,
    };
    approve(
        client,
        approver,
        ApproverRequest::Decide {
            request_digest,
            decision,
        },
    )
    .await
    .map(drop)
}

/// Certify the device behind `signer` with the root the page holds, for the
/// host at `client`: its certificate must be a host of the same root.
///
/// # Errors
///
/// [`CertifyError::Transport`] when the host is not reached,
/// [`CertifyError::Denied`] when it is not a host of this root, or the errors
/// of [`issue_device_cert`].
pub async fn certify_with_root(
    client: &dyn HttpClient,
    root: &dyn DeviceSigner,
    signer: &dyn DeviceSigner,
    label: &str,
) -> Result<(DeviceCert, DeviceCert), CertifyError> {
    let host_cert = https::host_info(client).await?.host_cert;
    check_host(&host_cert, root.device_public_key())?;
    let cert =
        issue_device_cert(root, signer.device_public_key(), label, IssuedRole::Browser).await?;
    Ok((cert, host_cert))
}

fn check_host(host_cert: &DeviceCert, user_public_key: [u8; 32]) -> Result<(), CertifyError> {
    verify_device_cert_at(host_cert, tilo_clock::unix_now_ns())
        .map_err(|error| CertifyError::Denied(format!("host certificate refused: {error}")))?;
    if host_cert.user_public_key != user_public_key || !host_cert.capabilities.can_act_as_host {
        return Err(CertifyError::Denied(
            "host certificate is not a host of this user".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        AgentScope, CERTIFICATION_SCHEMA_VERSION, CertificationRequest, RevocationList,
        verify_device_cert, verify_issued_cert,
    };

    #[tokio::test]
    async fn a_page_root_issues_browser_node_and_session_certificates_but_never_its_own() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let device = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();

        let browser = issue_device_cert(&root, device, "Browser", IssuedRole::Browser)
            .await
            .unwrap();
        verify_device_cert(&browser).unwrap();
        assert_eq!(browser.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(browser.capabilities, DeviceCapabilities::member());
        let lifetime = browser.expires_at_ns.unwrap() - browser.issued_at_ns;
        assert_eq!(lifetime, CertificationPolicy::browser().lifetime_ns);

        let node = issue_device_cert(&root, device, "Company node", IssuedRole::Node)
            .await
            .unwrap();
        verify_device_cert(&node).unwrap();
        let caps = node.capabilities;
        assert!(caps.can_act_as_host && caps.can_own_workspaces && caps.can_read_all_workspaces);
        assert!(!caps.can_admit_devices && !caps.foreign);
        assert_eq!(
            node.expires_at_ns.unwrap() - node.issued_at_ns,
            NODE_CERT_LIFETIME_NS
        );

        let session = issue_device_cert(&root, device, "Apps on the node", IssuedRole::Session)
            .await
            .unwrap();
        verify_device_cert(&session).unwrap();
        assert_eq!(session.capabilities, DeviceCapabilities::member());
        assert!(!session.capabilities.can_act_as_host);
        assert_eq!(
            session.expires_at_ns.unwrap() - session.issued_at_ns,
            NODE_CERT_LIFETIME_NS
        );

        let own = root.verifying_key().to_bytes();
        assert!(matches!(
            issue_device_cert(&root, own, "Root", IssuedRole::Node).await,
            Err(CertifyError::Request(_))
        ));
    }

    #[tokio::test]
    async fn the_root_certifies_an_agent_grant_until_it_expires_and_never_past_its_bound() {
        let root = SigningKey::from_bytes(&[3; 32]);
        let agent = SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes();
        let now = tilo_clock::unix_now_ns();
        let week = 7 * 24 * 60 * 60 * 1_000_000_000;
        let scope = AgentScope::Workspace([9; 16]);
        let grant = issue_agent_grant(
            &root,
            agent,
            "Claude Code via door.example",
            DeviceCapabilities::agent_grant(scope),
            Some(now + week),
        )
        .await
        .unwrap();
        assert_eq!(verify_device_cert_at(&grant, grant.issued_at_ns), Ok(()));
        assert_eq!(grant.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(grant.capabilities, DeviceCapabilities::agent_grant(scope));
        assert!(grant.capabilities.agent_grant_holds());
        assert_eq!(grant.expires_at_ns, Some(now + week));
        assert_eq!(grant.device_label, "Claude Code via door.example");
        assert!(verify_device_cert_at(&grant, now + week).is_err());

        let all = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
        for expiry in [
            Some(now + MAX_AGENT_GRANT_LIFETIME_NS + 60_000_000_000),
            Some(now - 1),
            None,
        ] {
            assert!(matches!(
                issue_agent_grant(&root, agent, "Claude Code", all.clone(), expiry).await,
                Err(CertifyError::Request(_))
            ));
        }
        assert!(matches!(
            issue_agent_grant(
                &root,
                agent,
                "Claude Code",
                DeviceCapabilities::member(),
                Some(now + week)
            )
            .await,
            Err(CertifyError::Request(_))
        ));
        assert!(matches!(
            issue_agent_grant(
                &root,
                root.verifying_key().to_bytes(),
                "Claude Code",
                all.clone(),
                Some(now + week)
            )
            .await,
            Err(CertifyError::Request(_))
        ));
        assert!(matches!(
            issue_agent_grant(&root, agent, " ", all.clone(), Some(now + week)).await,
            Err(CertifyError::Request(_))
        ));
        assert!(matches!(
            issue_agent_grant(&root, agent, &"x".repeat(81), all, Some(now + week)).await,
            Err(CertifyError::Request(_))
        ));
    }

    #[tokio::test]
    async fn the_root_certifies_a_server_key_that_lasts_until_revoked_or_its_chosen_expiry() {
        let root = SigningKey::from_bytes(&[3; 32]);
        let key = SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes();
        let now = tilo_clock::unix_now_ns();
        let year = 365 * 24 * 60 * 60 * 1_000_000_000;
        for expiry in [None, Some(now + year)] {
            let cert = issue_agent_grant(
                &root,
                key,
                "Visa backend",
                DeviceCapabilities::server_key_grant(),
                expiry,
            )
            .await
            .unwrap();
            assert_eq!(cert.expires_at_ns, expiry);
            assert_eq!(verify_device_cert_at(&cert, now + year - 1), Ok(()));
        }
        assert!(matches!(
            issue_agent_grant(
                &root,
                key,
                "Visa backend",
                DeviceCapabilities::server_key_grant(),
                Some(now - 1)
            )
            .await,
            Err(CertifyError::Request(_))
        ));
    }

    #[tokio::test]
    async fn the_root_makes_a_host_key_its_stand_in_whose_certificates_check_as_a_browser() {
        let root = SigningKey::from_bytes(&[5; 32]);
        let stand_in = SigningKey::from_bytes(&[6; 32]);
        let now = tilo_clock::unix_now_ns();
        let day = 24 * 60 * 60 * 1_000_000_000;
        let cert = issue_stand_in(
            &root,
            stand_in.verifying_key().to_bytes(),
            "Stand-in on tilo.example",
            30 * day,
        )
        .await
        .unwrap();
        assert_eq!(
            tilo_user_identity::verify_stand_in_cert_at(&cert, cert.issued_at_ns),
            Ok(())
        );
        assert!(
            verify_device_cert(&cert).is_err(),
            "a stand-in is no device"
        );
        assert_eq!(cert.capabilities, DeviceCapabilities::stand_in_approver());

        // What the stand-in approves checks as a browser this page asked for.
        let browser = SigningKey::from_bytes(&[7; 32]);
        let issued_at = cert.issued_at_ns + 1;
        let request = CertificationRequest {
            version: CERTIFICATION_SCHEMA_VERSION,
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: browser.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            requested: DeviceCapabilities::member(),
            nonce: [8; 16],
            invite: [0; 16],
            issued_at_ns: issued_at,
            expires_at_ns: issued_at + 60_000_000_000,
            signature: [0; 64],
        }
        .sign(&browser)
        .unwrap();
        let issued = tilo_user_identity::issue_device_cert_as_stand_in(
            &request,
            &stand_in,
            &cert,
            &RevocationList::new(),
            issued_at,
        )
        .unwrap();
        verify_issued_cert(
            &request,
            &issued,
            &CertificationPolicy::browser(),
            &RevocationList::new(),
            issued_at,
        )
        .unwrap();

        assert_eq!(cert.expires_at_ns, Some(cert.issued_at_ns + 30 * day));
        assert!(cert.issued_at_ns >= now);
        for lifetime in [0, 30 * day + 1] {
            assert!(matches!(
                issue_stand_in(
                    &root,
                    stand_in.verifying_key().to_bytes(),
                    "Stand-in",
                    lifetime
                )
                .await,
                Err(CertifyError::Request(_))
            ));
        }
        assert!(matches!(
            issue_stand_in(&root, root.verifying_key().to_bytes(), "Stand-in", day).await,
            Err(CertifyError::Request(_))
        ));
    }

    /// Revoking the host's stand-in once the person keeps their root with a
    /// passkey refuses what it would approve from then on, and keeps every
    /// device it approved before a device of theirs.
    #[tokio::test]
    async fn a_revoked_stand_in_approves_nothing_after_and_what_it_approved_before_stays_valid() {
        let root = SigningKey::from_bytes(&[5; 32]);
        let stand_in = SigningKey::from_bytes(&[6; 32]);
        let day = 24 * 60 * 60 * 1_000_000_000;
        let cert = issue_stand_in(
            &root,
            stand_in.verifying_key().to_bytes(),
            "Stand-in on tilo.example",
            30 * day,
        )
        .await
        .unwrap();
        let request = |key: u8, at: u64| {
            let browser = SigningKey::from_bytes(&[key; 32]);
            CertificationRequest {
                version: CERTIFICATION_SCHEMA_VERSION,
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: browser.verifying_key().to_bytes(),
                device_label: "Browser".into(),
                requested: DeviceCapabilities::member(),
                nonce: [key; 16],
                invite: [0; 16],
                issued_at_ns: at,
                expires_at_ns: at + 60_000_000_000,
                signature: [0; 64],
            }
            .sign(&browser)
            .unwrap()
        };
        // A browser the stand-in approves while the root is still in this tab.
        let before = request(7, cert.issued_at_ns + 1);
        let approved = tilo_user_identity::issue_device_cert_as_stand_in(
            &before,
            &stand_in,
            &cert,
            &RevocationList::new(),
            before.issued_at_ns,
        )
        .unwrap();

        let revoked_at = cert.issued_at_ns + 2;
        let entry = revoke_device(
            &root,
            stand_in.verifying_key().to_bytes(),
            revoked_at,
            RevocationReason::Retired,
        )
        .await
        .unwrap();
        tilo_user_identity::verify_revocation_entry(&entry).unwrap();
        assert_eq!(entry.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(entry.reason, RevocationReason::Retired);
        let mut revocations = RevocationList::new();
        revocations.insert(entry).unwrap();

        assert!(
            !revocations.refuses(&approved),
            "a device the stand-in approved before the revocation stays a device of the person"
        );
        assert!(revocations.refuses(&cert), "the stand-in itself is refused");
        let later = request(8, revoked_at);
        assert_eq!(
            tilo_user_identity::issue_device_cert_as_stand_in(
                &later,
                &stand_in,
                &cert,
                &revocations,
                later.issued_at_ns,
            ),
            Err(tilo_user_identity::CertificationError::Revoked),
            "a revoked stand-in approves nothing from its revocation on"
        );
        assert!(matches!(
            revoke_device(
                &root,
                root.verifying_key().to_bytes(),
                1,
                RevocationReason::Retired
            )
            .await,
            Err(CertifyError::Request(_))
        ));
    }
}
