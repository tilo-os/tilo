//! Personal-device pairing links for an enrollment endpoint with no web host.

use std::net::SocketAddr;

use anyhow::{Context as _, bail};
use tilo_identity::certification_ceremony::pairing_code;
use tilo_user_identity::{
    EnrollmentScope, decode_enrollment_token, encode_enrollment_token, enrollment_scope,
    mint_personal_enrollment_token,
};

use crate::approval::ApprovalIdentity;
use crate::request::{JoinOptions, JoinedDevice, PairingProgress, install_answer, prepare_join};
use crate::transport::{ALPN, RequestExchange, address_of, dial_of, endpoint_for, parse_relay};

const PREFIX: &str = "tilo://pair-device?token=";
const LIFETIME_NS: u64 = 600_000_000_000;
const MAX_ADDRESSES: usize = 8;

/// The live endpoint and device authority that can issue a link to it.
pub struct Issuer {
    endpoint: iroh::Endpoint,
    device: ed25519_dalek::SigningKey,
    person: [u8; 32],
    certified_at_ns: u64,
}

impl Issuer {
    /// Use the same device key that verified its authority to serve enrollment.
    #[must_use]
    pub fn new(endpoint: &iroh::Endpoint, identity: &ApprovalIdentity) -> Self {
        Self {
            endpoint: endpoint.clone(),
            device: identity.device.clone(),
            person: identity.user_public_key,
            certified_at_ns: identity.issuer_cert.issued_at_ns,
        }
    }

    /// Make a ten-minute link a new device can present when asking for approval.
    ///
    /// # Errors
    ///
    /// This endpoint cannot be reached or serves a different person.
    pub fn issue(&self, person: [u8; 32], now_ms: u64) -> anyhow::Result<(String, u64)> {
        if self.person != person {
            bail!("the enrollment endpoint serves a different person");
        }
        let dial = dial_of(&self.endpoint);
        let address = self.endpoint.addr();
        let direct: Vec<_> = address.ip_addrs().take(MAX_ADDRESSES).collect();
        if direct.is_empty() && dial.relay_url.is_empty() {
            bail!("the enrollment endpoint has no reachable address yet");
        }
        let issued_at = now_ms.saturating_mul(1_000_000);
        let expiry = issued_at.saturating_add(LIFETIME_NS);
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)
            .map_err(|error| anyhow::anyhow!("make an enrollment nonce: {error}"))?;
        let token = encode_enrollment_token(&mint_personal_enrollment_token(
            person,
            crate::validity_start_ns(issued_at).max(self.certified_at_ns),
            expiry,
            nonce,
            vec![dial],
            &self.device,
        )?)?;
        let mut link = format!("{PREFIX}{token}");
        for address in direct {
            use std::fmt::Write as _;
            write!(link, "&at={address}")?;
        }
        Ok((link, expiry / 1_000_000))
    }
}

/// Redeem a personal-device link using the native enrollment exchange and
/// install only after the approving device confirms the certificate.
///
/// # Errors
///
/// The link is invalid, names an unreachable device, or is refused.
pub fn redeem(
    link: &str,
    options: JoinOptions<'_>,
    mut progress: impl FnMut(PairingProgress<'_>) -> anyhow::Result<()>,
) -> anyhow::Result<JoinedDevice> {
    let mut parts = link
        .trim()
        .strip_prefix(PREFIX)
        .filter(|text| text.len() <= 4096)
        .context("expected a Tilo device pairing link")?
        .split('&');
    let token = parts.next().context("the link has no enrollment token")?;
    let direct = parts
        .map(|part| {
            part.strip_prefix("at=")
                .context("invalid address in device pairing link")?
                .parse::<SocketAddr>()
                .context("invalid address in device pairing link")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if direct.len() > MAX_ADDRESSES {
        bail!("the device pairing link has too many addresses");
    }
    let options = JoinOptions {
        kind: crate::EnrollmentKind::PersonalDevice,
        ..options
    };
    ensure_first_use(options)?;
    if enrollment_scope(&decode_enrollment_token(token)?, crate::now_ns()?)?
        != EnrollmentScope::PersonalDevice
    {
        bail!("this token is not a personal device pairing link");
    }
    let prepared = prepare_join(token, options)?;
    let [dial] = prepared.token.dial.as_slice() else {
        bail!("the device pairing link must name one approving device");
    };
    // Only direct routing hints come from the URL. The signed token pins the
    // endpoint and the request is sealed to its issuing device.
    let mut address = address_of(dial)?;
    for direct in direct {
        address = address.with_ip_addr(direct);
    }
    let relays = (!dial.relay_url.is_empty())
        .then(|| parse_relay(&dial.relay_url).map(iroh::RelayMap::from))
        .transpose()?;
    let code = pairing_code(
        prepared.request.device_public_key,
        prepared.request.server_node_id,
    );
    progress(PairingProgress::Waiting {
        code: &code,
        label: &prepared.request.label,
    })?;
    let runtime = crate::transport::runtime()?;
    let window = crate::socket_timeout(options.timeout);
    let (endpoint, connection) = runtime.block_on(async {
        let endpoint = endpoint_for(&prepared.device, relays.as_ref()).await?;
        let connection = tokio::time::timeout(window, endpoint.connect(address, ALPN))
            .await
            .context("the enrollment window closed")?
            .context("could not reach the approving device")?;
        Ok::<_, anyhow::Error>((endpoint, connection))
    })?;
    let joined = RequestExchange {
        token: &prepared.token,
        request: &prepared.request,
        request_bytes: &prepared.request_bytes,
        response_secret: &prepared.response_secret,
    }
    .install(
        &runtime,
        &connection,
        window,
        |payload, response, confirm| {
            ensure_first_use(options)?;
            let record_path = install_answer(&prepared, options, payload, response, confirm)?
                .context("the approval did not install a personal identity")?;
            Ok((record_path, payload.cert.device_label.clone()))
        },
    );
    runtime.block_on(endpoint.close());
    let (record_path, label) = joined?;
    Ok(JoinedDevice {
        code,
        identity: prepared.token.user_public_key,
        record_path,
        label,
    })
}

fn ensure_first_use(options: JoinOptions<'_>) -> anyhow::Result<()> {
    let paths = tilo_device_identity::IdentityPaths::under(options.identity_root);
    let person = options
        .identity_dir
        .context("the device needs a local identity directory")?;
    if paths.device_cert.try_exists()?
        || tilo_identity_client::resolve_active_identity(person).is_some()
    {
        bail!("this device already has an identity");
    }
    Ok(())
}
