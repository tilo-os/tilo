//! Specific external moderation completion under retained action custody.
use super::{Arc, CapnpBrokerState, CapnpChannelHandle, OriginalInvocation};
use registry_types::ModerationCase;
use runtime_collab::external_moderation::{MAX_WIRE_BYTES, Request, validate_receipt};
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::sync::Weak;
use std::time::Duration;

type Reply = Result<ModerationCase, String>;
pub(super) enum Entry {
    Running,
    Complete(Reply),
}
#[derive(Default)]
pub(super) struct Registry(HashMap<u64, Entry>);

struct Context {
    original: Arc<OriginalInvocation>,
    state: Weak<CapnpBrokerState>,
    request: Request,
    document: super::NativeDocumentReadContext,
}
impl Context {
    fn recheck(&self) -> Result<(), String> {
        let state = self
            .state
            .upgrade()
            .ok_or("external moderation broker retired")?;
        self.original.check_execution_frontier(&state)?;
        self.original.check_resource_publication(&state)?;
        let (_, published) = state
            .published_action(
                self.original.route(),
                &self.original.request().address.resource,
                &self.original.request().address.action,
            )
            .ok_or("external moderation publication retired")?;
        if published.operation_kind != self.original.dispatch_spec().kind
            || published.contract != *self.original.contract()
        {
            return Err("external moderation current action contract mismatch".into());
        }
        self.original.check_lifecycle()?;
        let now = tilo_clock::unix_now();
        if now < self.request.operation.issued_at_unix
            || now >= self.request.operation.expires_at_unix
        {
            return Err("external moderation operation expired or not yet valid".into());
        }
        trusted_destination(&self.request.operation.destination)?;
        self.check_committed_evidence(&state)?;
        self.original.check_execution_frontier(&state)
    }

    fn check_committed_evidence(&self, state: &CapnpBrokerState) -> Result<(), String> {
        use super::NativeDocumentRead;
        use runtime_collab::committed_document::MAX_DOCUMENT_BYTES;

        self.document.revalidate()?;
        let service = state
            .services
            .committed_documents
            .as_ref()
            .ok_or("external moderation committed report owner unavailable")?;
        service.authorize(&self.document)?;
        let _reservation = state.committed_read_budget.reserve()?;
        let captured = service.capture(&self.document, None, MAX_DOCUMENT_BYTES)?;
        let NativeDocumentRead::Captured { body, .. } = captured else {
            return Err("external moderation committed report unavailable".into());
        };
        if body.bytes().len() > MAX_DOCUMENT_BYTES {
            return Err("external moderation committed report exceeds limit".into());
        }
        let document = automerge::Automerge::load(body.bytes())
            .map_err(|_| "external moderation committed report is invalid")?;
        let argument = self
            .original
            .request()
            .argument
            .as_deref()
            .ok_or("external moderation original report arguments unavailable")?;
        let argument: serde_json::Value = serde_json::from_str(
            argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(argument),
        )
        .map_err(|_| "external moderation invalid original report arguments")?;
        let app = argument
            .get("app")
            .and_then(serde_json::Value::as_str)
            .ok_or("external moderation original report app unavailable")?;
        validate_committed_report(&document, app, &self.request.operation)?;
        service.authorize(&self.document)?;
        self.document.revalidate()
    }

    fn run(&self) -> Reply {
        self.recheck()?;
        // Parse, but never normalize the approved destination used in the signature.
        let url = destination_url(&self.request.operation.destination.base_url)?;
        let client = tilo_net::http::blocking_client()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        // The identity is loaded afresh for every operation. Revocation, identity
        // selection and device removal invalidate custody; no signing key is cached.
        let (device, certificate) =
            tilo_identity_host::broker::trusted_current_device(&self.original.route().cluster_id)
                .map_err(|error| error.to_string())?;
        if certificate.user_public_key != self.original.certificate()?.user_public_key {
            return Err("external moderation cannot use another person's signing identity".into());
        }
        self.recheck()?;
        let signed = registry_types::sign_external_moderation(&self.request.operation, &device)
            .map_err(|error| error.to_string())?;
        drop(device);
        self.recheck()?;
        // Re-read enrollment immediately before disclosure as well as signing.
        let (_, current) =
            tilo_identity_host::broker::trusted_current_device(&self.original.route().cluster_id)
                .map_err(|error| error.to_string())?;
        if certificate != current {
            return Err("external moderation signing identity changed".into());
        }
        let response = client
            .post(url)
            .json(&signed)
            .send()
            .map_err(|error| error.to_string())?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_WIRE_BYTES as u64)
        {
            return Err("external moderation response exceeds limit".into());
        }
        let mut bytes = Vec::new();
        response
            .take((MAX_WIRE_BYTES as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err("external moderation response exceeds limit".into());
        }
        if !status.is_success() {
            return Err(format!("registry refused moderation request ({status})"));
        }
        let case: ModerationCase =
            serde_json::from_slice(&bytes).map_err(|_| "invalid external moderation receipt")?;
        validate_receipt(&self.request, &case)?;
        self.recheck()?;
        Ok(case)
    }
}

fn validate_committed_report(
    document: &automerge::Automerge,
    app: &str,
    operation: &registry_types::ExternalModerationOperation,
) -> Result<(), String> {
    use automerge::{ObjType, ReadDoc as _, Value};
    use registry_types::ModerationMutation;
    let object = |parent: &automerge::ObjId, key: &str| {
        let values = document.get_all(parent, key).map_err(|e| e.to_string())?;
        match values.as_slice() {
            [(Value::Object(ObjType::Map), object)] => Ok(object.clone()),
            _ => Err("external moderation committed report absent or ambiguous".to_string()),
        }
    };
    let reports = object(&automerge::ROOT, "reports")?;
    let report = object(&reports, app)?;
    let text = |key: &str| -> Result<Option<String>, String> {
        let values = document.get_all(&report, key).map_err(|e| e.to_string())?;
        match values.as_slice() {
            [] => Ok(None),
            [(Value::Scalar(value), _)] if value.is_null() => Ok(None),
            [(Value::Scalar(value), _)] => value
                .to_str()
                .map(|s| Some(s.to_owned()))
                .ok_or_else(|| "external moderation invalid committed report text".into()),
            [(Value::Object(ObjType::Text), object)] => {
                document.text(object).map(Some).map_err(|e| e.to_string())
            }
            _ => Err("external moderation ambiguous committed report text".into()),
        }
    };
    let note = text("note")?.unwrap_or_default();
    let status = text("status")?.unwrap_or_default();
    let case = text("case")?;
    let digest = registry_types::report_evidence_digest(app, &status, &note, case.as_deref())
        .map_err(|error| error.to_string())?;
    let matches = match &operation.mutation {
        ModerationMutation::FileReport {
            package_id,
            category,
            summary,
            evidence_digest_hex,
        } => {
            package_id == app
                && category == "policy"
                && summary == note.trim()
                && status == "Not submitted"
                && case.is_none()
                && evidence_digest_hex == &digest
        }
        ModerationMutation::PublisherAppeal {
            case_id,
            reason,
            evidence_digest_hex,
        } => {
            case.as_deref() == Some(case_id.as_str())
                && reason == &note
                && evidence_digest_hex
                    .as_ref()
                    .is_none_or(|value| value == &digest)
        }
        _ => false,
    };
    if !matches {
        return Err(
            "external moderation differs from the original committed report evidence".into(),
        );
    }
    Ok(())
}

/// The operation names a destination, not a trust anchor. Read the host's
/// current configuration on every frontier check so disabling a registry or
/// rotating its pin also retires already admitted requests.
fn trusted_destination(destination: &registry_types::ModerationDestination) -> Result<(), String> {
    destination_url(&destination.base_url)?;
    let configured = app_cli::config::load_registries_no_seed();
    if !configured.enabled().any(|registry| {
        registry.base_url == destination.base_url
            && registry
                .owner_pubkey_hex
                .eq_ignore_ascii_case(&destination.owner_pubkey_hex)
            && registry.owner_pubkey_hex != app_cli::config::placeholder_owner_pubkey()
            && hex::decode(&registry.owner_pubkey_hex).is_ok_and(|key| key.len() == 32)
    }) {
        return Err(
            "external moderation destination and owner are not a trusted configured registry"
                .into(),
        );
    }
    Ok(())
}

fn destination_url(base: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base)
        .map_err(|_| "registry destination must be credential-free HTTPS")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("registry destination must be credential-free HTTPS".into());
    }
    url.set_path(&format!(
        "{}/moderation/mutations",
        url.path().trim_end_matches('/')
    ));
    Ok(url)
}

pub(super) fn is_envelope(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_WIRE_BYTES {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|value| {
        value.get("protocol").and_then(serde_json::Value::as_str)
            == Some(runtime_collab::external_moderation::PROTOCOL)
    })
}

pub(super) fn start(
    channel: &CapnpChannelHandle,
    bytes: &[u8],
    parent: &[u8],
) -> Result<tokio::sync::oneshot::Receiver<Reply>, String> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err("external moderation request exceeds limit".into());
    }
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| "invalid external moderation request")?;
    request.validate()?;
    let invocation =
        uuid::Uuid::parse_str(&request.invocation).map_err(|_| "invalid moderation invocation")?;
    if parent != invocation.as_bytes() {
        return Err("external moderation continuation invocation differs".into());
    }
    let context = Context {
        original: OriginalInvocation::for_external_moderation(channel, &request)?,
        state: Arc::downgrade(&channel.state),
        document: channel.document_read_context()?,
        request,
    };
    context.recheck()?;
    let state = &channel.state;
    let id = context.original.request().wire_id;
    let retained: HashSet<_> = state
        .pending_invocations
        .lock()
        .map_err(|_| "invocation registry unavailable")?
        .keys()
        .copied()
        .collect();
    let cached = {
        let mut registry = state
            .external_moderation_jobs
            .lock()
            .map_err(|_| "moderation registry unavailable")?;
        registry
            .0
            .retain(|id, entry| retained.contains(id) || matches!(entry, Entry::Running));
        match registry.0.get(&id) {
            Some(Entry::Running) => {
                return Err(
                    "external moderation already executing; observe the original invocation".into(),
                );
            }
            Some(Entry::Complete(reply)) => Some(reply.clone()),
            None => {
                if registry.0.len() >= 256 {
                    return Err("external moderation retained execution limit reached".into());
                }
                registry.0.insert(id, Entry::Running);
                None
            }
        }
    };
    let (send, receive) = tokio::sync::oneshot::channel();
    if let Some(reply) = cached {
        let _ = send.send(reply);
        return Ok(receive);
    }
    let state = Arc::downgrade(state);
    let spawned = std::thread::Builder::new()
        .name("tilo-external-moderation".into())
        .spawn(move || {
            let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.run()))
                .unwrap_or_else(|_| {
                    Err("external moderation worker panicked; outcome unobserved".into())
                });
            if let Some(state) = state.upgrade()
                && let Ok(mut registry) = state.external_moderation_jobs.lock()
            {
                registry.0.insert(id, Entry::Complete(reply.clone()));
            }
            let _ = send.send(reply);
        });
    if let Err(error) = spawned {
        channel
            .state
            .external_moderation_jobs
            .lock()
            .map_err(|_| "moderation registry unavailable")?
            .0
            .insert(id, Entry::Complete(Err(error.to_string())));
        return Err(error.to_string());
    }
    Ok(receive)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_moderation_destination_requires_https_without_credentials_or_redirect_hints() {
        for base in [
            "http://registry.example",
            "https://user@registry.example",
            "file:///registry",
            "https://registry.example?q=1",
            "https://registry.example/#fragment",
        ] {
            assert!(destination_url(base).is_err(), "{base}");
        }
        assert_eq!(
            destination_url("https://registry.example/base/")
                .unwrap()
                .as_str(),
            "https://registry.example/base/moderation/mutations"
        );
    }
}
