//! Issue a certificate where the person already holds the root and has the
//! device key from local sign-in or an explicit enrollment handoff.

use crate::{CertifyError, DeviceSigner};
use tilo_user_identity::{
    AgentScope, CertificationPolicy, DeviceCapabilities, DeviceCert,
    certification::{CERTIFICATION_CLOCK_ALLOWANCE_NS, MAX_CERTIFICATION_LABEL_BYTES},
    check_agent_grant, sign_device_cert_async, verify_device_cert_at,
};

/// How long a node's certificate lasts.
pub const NODE_CERT_LIFETIME_NS: u64 = 365 * 24 * 60 * 60 * 1_000_000_000;

/// Which device a certificate the person's root issues is for: every one is
/// a member device of the person, and how long it lasts is the difference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuedRole {
    /// A browser: [`CertificationPolicy::browser`] (30 days).
    Browser,
    /// A machine the person set up: one year.
    Device,
}

impl IssuedRole {
    fn lifetime_ns(self) -> u64 {
        match self {
            Self::Browser => CertificationPolicy::browser().lifetime_ns,
            Self::Device => NODE_CERT_LIFETIME_NS,
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
    let now = tilo_clock::unix_now_ns();
    issue_member_cert(root, device_public_key, label, role, now, now).await
}

/// Have `root` certify `device_public_key`, this browser's own device key,
/// for the proof a service signs its person in with
/// ([`sign_in_proof`](crate::sign_in_proof)).
///
/// The service reads the certificate by its own clock the moment the answer
/// arrives, and that clock may be behind this device's. So the certificate is
/// dated as a paired device's is
/// ([`prepare_device_cert`](tilo_user_identity::prepare_device_cert)): valid
/// from [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] before this device's clock, and a
/// browser's lifetime long from there. The service keeps its exact check:
/// a device further ahead of it than that is still refused, and there is no
/// second allowance.
///
/// # Errors
///
/// As [`issue_device_cert`].
pub async fn issue_sign_in_cert(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    label: &str,
) -> Result<DeviceCert, CertifyError> {
    let now = tilo_clock::unix_now_ns();
    let starts = read_at_once_elsewhere(now);
    issue_member_cert(
        root,
        device_public_key,
        label,
        IssuedRole::Browser,
        now,
        starts,
    )
    .await
}

/// When a certificate issued at `now_ns` starts, if another machine reads it
/// by its own clock the moment it arrives:
/// [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] earlier, the one rule for how far two
/// clocks may differ. Every verifier keeps its exact check.
const fn read_at_once_elsewhere(now_ns: u64) -> u64 {
    now_ns.saturating_sub(CERTIFICATION_CLOCK_ALLOWANCE_NS)
}

/// [`issue_device_cert`] at `now_ns`: a certificate valid from `starts_ns`
/// that lasts `role`'s lifetime from there.
async fn issue_member_cert(
    root: &dyn DeviceSigner,
    device_public_key: [u8; 32],
    label: &str,
    role: IssuedRole,
    now_ns: u64,
    starts_ns: u64,
) -> Result<DeviceCert, CertifyError> {
    let user_public_key = root.device_public_key();
    if device_public_key == user_public_key {
        return Err(CertifyError::Request(
            "the root certifies devices, never itself".into(),
        ));
    }
    let cert = DeviceCert {
        user_public_key,
        device_public_key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: starts_ns,
        expires_at_ns: Some(starts_ns.saturating_add(role.lifetime_ns())),
        signature: [0; 64],
        issuer: None,
    };
    let cert = signed(root, cert).await?;
    verify_device_cert_at(&cert, now_ns)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    Ok(cert)
}

/// `cert` as `root` signs it, every check the crate applies first.
async fn signed(root: &dyn DeviceSigner, mut cert: DeviceCert) -> Result<DeviceCert, CertifyError> {
    sign_device_cert_async(&mut cert, root)
        .await
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    Ok(cert)
}

/// Have `root` certify `device_public_key` as an agent's grant with
/// `capabilities`, expiring at `expires_at_ns`: a service runs the agent for
/// the person in exactly those workspaces, until then. Only a server key may
/// last until revoked (`None`); a grant admits no devices and has no life of
/// its own to extend.
///
/// The service that holds the grant's key reads the certificate by its own
/// clock the moment the answer arrives, so it is dated as
/// [`issue_sign_in_cert`]'s is: valid from
/// [`CERTIFICATION_CLOCK_ALLOWANCE_NS`] before this device's clock. It ends
/// when it was asked to, and the most a grant may last counts from that
/// earlier start, so the allowance lengthens no grant.
///
/// # Errors
///
/// [`CertifyError::Request`] for the root's own key, capabilities that are no
/// grant's, an expiry not in the future or (but for a server key) missing or
/// beyond [`MAX_AGENT_GRANT_LIFETIME_NS`](tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS) from the certificate's start, a
/// label a certificate does not carry, or a signature that does not verify.
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
    let starts = read_at_once_elsewhere(now);
    // The rule every verifier applies to a grant, from the one place it is written:
    // its expiry lies ahead of this device's clock, and it lasts no longer than a
    // grant may from the start its certificate carries.
    check_agent_grant(&capabilities, now, expires_at_ns)
        .and_then(|()| check_agent_grant(&capabilities, starts, expires_at_ns))
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let cert = DeviceCert {
        user_public_key,
        device_public_key,
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: starts,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    let cert = signed(root, cert).await?;
    verify_device_cert_at(&cert, now).map_err(|e| request(&e))?;
    Ok(cert)
}

/// What a service's sign-in asks the person's Tilo to sign: the service's origin and its
/// challenge, the client that asks, and the grant the person lets it hold, when it asks for one.
pub struct SignInAsk<'a> {
    pub audience: &'a str,
    pub challenge: [u8; 32],
    pub client: &'a str,
    pub grant: Option<GrantAsk>,
}

/// The grant a sign-in asks the root to certify: the key the service generated for it, what it
/// reaches, until when, whether it is a key the person makes for their own backend
/// (`server_key`, which alone may last until revoked), and whether it operates web pages in
/// Browser without asking each time.
pub struct GrantAsk {
    pub key: [u8; 32],
    pub scope: AgentScope,
    pub expires_at_ns: Option<u64>,
    pub server_key: bool,
    pub browser_pages: bool,
}

/// What the person's Tilo answers a sign-in with: the proof, and the grant's certificate when
/// one was asked for, each as the service decodes it ([`encode`](crate::encode)).
pub struct SignedIn {
    pub proof: String,
    pub grant: Option<String>,
}

/// The person's answer to `ask`, wherever they approve it: `root` certifies `device` (called
/// `device_label`) for the proof alone ([`issue_sign_in_cert`]), the device proves the person
/// to the service, and the root certifies the grant's key ([`issue_agent_grant`]). The one
/// signer of a sign-in, for the page and for a computer that holds the root alike.
///
/// # Errors
///
/// [`CertifyError::Request`] for a grant of an agent that names no expiry, a label or expiry
/// the root refuses, or a signature that fails.
pub async fn answer_sign_in(
    root: &dyn DeviceSigner,
    device: &dyn DeviceSigner,
    device_label: &str,
    ask: &SignInAsk<'_>,
) -> Result<SignedIn, CertifyError> {
    let request = |error: &dyn std::fmt::Display| CertifyError::Request(error.to_string());
    let cert = issue_sign_in_cert(root, device.device_public_key(), device_label).await?;
    let proof = crate::sign_in_proof(cert, device, ask.audience, ask.challenge)
        .await
        .map_err(|error| request(&error))?;
    let proof = crate::encode(&proof).map_err(|error| request(&error))?;
    let Some(grant) = &ask.grant else {
        return Ok(SignedIn { proof, grant: None });
    };
    let capabilities = if grant.server_key {
        DeviceCapabilities::own_key_grant(grant.scope.clone())
    } else if grant.expires_at_ns.is_some() {
        DeviceCapabilities::agent_grant(grant.scope.clone())
    } else {
        return Err(CertifyError::Request(
            "an agent's grant lasts until a time it names; only a server key may last until \
             revoked"
                .into(),
        ));
    };
    let cert = issue_agent_grant(
        root,
        grant.key,
        &crate::grant_label(ask.client, ask.audience),
        capabilities.with_browser_pages(grant.browser_pages),
        grant.expires_at_ns,
    )
    .await?;
    let grant = crate::encode(&cert).map_err(|error| request(&error))?;
    Ok(SignedIn {
        proof,
        grant: Some(grant),
    })
}

/// Certify the device behind `signer` with the root the page holds, and
/// answer `server_cert`, the certificate the server it goes to presented,
/// beside it.
///
/// The root certifies a device of its own person, and whose server it goes to
/// does not enter into it: the person's own node, a server that keeps a
/// workspace they were invited into, and a hosted server that keeps their own
/// workspace all take the person's devices by the person's root, as the
/// workspaces they are a member of decide. The certificate names the person,
/// never the server's owner. So a page that joined another person's workspace
/// certifies its device again exactly as a page on its person's own node does.
///
/// # Errors
///
/// [`CertifyError::Denied`] when the server's own certificate does not verify,
/// or the errors of [`issue_device_cert`].
pub async fn certify_with_root(
    root: &dyn DeviceSigner,
    signer: &dyn DeviceSigner,
    label: &str,
    server_cert: &DeviceCert,
) -> Result<(DeviceCert, DeviceCert), CertifyError> {
    verify_device_cert_at(server_cert, tilo_clock::unix_now_ns())
        .map_err(|error| CertifyError::Denied(format!("server certificate refused: {error}")))?;
    let cert =
        issue_device_cert(root, signer.device_public_key(), label, IssuedRole::Browser).await?;
    Ok((cert, server_cert.clone()))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::arithmetic_side_effects,
        reason = "tests assert on successful issuance and exact clock boundaries"
    )]

    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        AgentScope, DeviceCertError, MAX_AGENT_GRANT_LIFETIME_NS, verify_device_cert,
    };

    /// The page's clock in the tests that hold it, in Unix milliseconds: a
    /// browser's clock reads whole milliseconds.
    const PAGE_MS: u64 = 1_800_000_000_000;

    /// `issue` with this thread's clock, the page's, held at [`PAGE_MS`].
    fn at_the_page<T>(issue: impl Future<Output = T>) -> T {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        tilo_clock::with_wall_ms(PAGE_MS, || runtime.block_on(issue))
    }

    #[test]
    fn what_a_service_reads_at_once_is_valid_from_the_clock_allowance_before_the_page_s_clock() {
        let page_ns = PAGE_MS * 1_000_000;
        let starts = page_ns - CERTIFICATION_CLOCK_ALLOWANCE_NS;
        let root = SigningKey::from_bytes(&[21; 32]);
        let device = SigningKey::from_bytes(&[22; 32]).verifying_key().to_bytes();
        let key = SigningKey::from_bytes(&[23; 32]).verifying_key().to_bytes();

        // The certificate a sign-in proof carries: a service whose clock is
        // the whole allowance behind the page's takes it, one a nanosecond
        // further behind refuses it as before, and it lasts no longer.
        let cert = at_the_page(issue_sign_in_cert(&root, device, "Tilo on the web")).unwrap();
        assert_eq!(cert.issued_at_ns, starts);
        assert_eq!(
            cert.expires_at_ns,
            Some(starts + CertificationPolicy::browser().lifetime_ns)
        );
        assert_eq!(cert.capabilities, DeviceCapabilities::member());
        assert_eq!(verify_device_cert_at(&cert, starts), Ok(()));
        assert_eq!(
            verify_device_cert_at(&cert, starts - 1),
            Err(DeviceCertError::NotYetValid)
        );
        let own = root.verifying_key().to_bytes();
        assert!(matches!(
            at_the_page(issue_sign_in_cert(&root, own, "Root")),
            Err(CertifyError::Request(_))
        ));

        // A certificate the root issues for a device of its own is dated by
        // this device's clock alone, as before.
        let browser = at_the_page(issue_device_cert(
            &root,
            device,
            "Browser",
            IssuedRole::Browser,
        ))
        .unwrap();
        assert_eq!(browser.issued_at_ns, page_ns);

        // An agent's grant likewise, and it ends when it was asked to.
        let all = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
        let week = 7 * 24 * 60 * 60 * 1_000_000_000;
        let grant = at_the_page(issue_agent_grant(
            &root,
            key,
            "Claude Code via api.example",
            all.clone(),
            Some(page_ns + week),
        ))
        .unwrap();
        assert_eq!(grant.issued_at_ns, starts);
        assert_eq!(grant.expires_at_ns, Some(page_ns + week));
        assert_eq!(verify_device_cert_at(&grant, starts), Ok(()));
        assert_eq!(
            verify_device_cert_at(&grant, starts - 1),
            Err(DeviceCertError::NotYetValid)
        );

        // The most a grant may last counts from the start its certificate
        // carries, so the allowance lengthens none; and an expiry that does
        // not lie ahead of the page's clock is no grant.
        let longest = starts + MAX_AGENT_GRANT_LIFETIME_NS;
        let lasting = |expiry| {
            at_the_page(issue_agent_grant(
                &root,
                key,
                "Claude Code",
                all.clone(),
                Some(expiry),
            ))
        };
        assert_eq!(lasting(longest).unwrap().expires_at_ns, Some(longest));
        assert!(matches!(
            lasting(longest + 1),
            Err(CertifyError::Request(_))
        ));
        assert!(matches!(lasting(page_ns), Err(CertifyError::Request(_))));
    }

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

        let node = issue_device_cert(&root, device, "Company node", IssuedRole::Device)
            .await
            .unwrap();
        verify_device_cert(&node).unwrap();
        assert_eq!(node.capabilities, DeviceCapabilities::member());
        assert_eq!(
            node.expires_at_ns.unwrap() - node.issued_at_ns,
            NODE_CERT_LIFETIME_NS
        );

        let own = root.verifying_key().to_bytes();
        assert!(matches!(
            issue_device_cert(&root, own, "Root", IssuedRole::Device).await,
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
            "Claude Code via entry.example",
            DeviceCapabilities::agent_grant(scope.clone()),
            Some(now + week),
        )
        .await
        .unwrap();
        assert_eq!(verify_device_cert_at(&grant, grant.issued_at_ns), Ok(()));
        assert_eq!(grant.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(
            grant.capabilities,
            DeviceCapabilities::agent_grant(scope.clone())
        );
        assert!(grant.capabilities.agent_grant_holds());
        assert_eq!(grant.expires_at_ns, Some(now + week));
        assert_eq!(grant.device_label, "Claude Code via entry.example");
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
}
