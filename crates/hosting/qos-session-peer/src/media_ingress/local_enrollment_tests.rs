//! Local OS-owner re-enrollment after a long publication history: the reply
//! stays within the 1 MiB inline bound by switching to the signed chunked
//! governance handoff, served over the real installed TLS ingress.
use super::data_tests::Harness;
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use hyper::StatusCode;
use qos_peer_state::enrollment::{ChallengeRequest, decode, sign};
use tilo_relay::delivery_v3::{
    media_wire::split_signature,
    onboarding_wire::{
        GovernanceBundle, HANDOFF_CHUNKS_PATH, HandoffChunk, HandoffManifest, HandoffPurpose,
        HandoffRequest, MAX_HANDOFF_RANGE_BYTES, OnboardingScope,
    },
};
use ui::automerge::transaction::Transactable;

/// `count` small incremental publications, as a browser editing session makes.
fn publish_edits(h: &mut Harness, count: usize, bytes_each: usize) {
    use rand::{RngCore, SeedableRng};
    let mut random = rand::rngs::StdRng::seed_from_u64(9051);
    for index in 0..count {
        let mut input = vec![0u8; bytes_each];
        random.fill_bytes(&mut input);
        h.f.current
            .with_owner(&mut h.f.owner, |owner| -> anyhow::Result<()> {
                let mut candidate = owner.peer().clone();
                let saved = candidate
                    .document("dev.qos.chat", "")
                    .ok_or_else(|| anyhow::anyhow!("chat record missing"))?;
                let mut document = ui::automerge::AutoCommit::load(&saved)?;
                let before = document.get_heads();
                document.put(
                    ui::automerge::ROOT,
                    format!("edit-{index}"),
                    ui::automerge::ScalarValue::Bytes(input),
                )?;
                document.commit();
                let delta = document.save_after(&before);
                candidate
                    .publish_incremental(
                        "dev.qos.chat",
                        "",
                        delta,
                        &owner.identity.native,
                        web_time::SystemTime::now(),
                    )
                    .map_err(anyhow::Error::msg)?;
                owner.commit_mutation_candidate(candidate, owner.directory().to_vec())?;
                Ok(())
            })
            .unwrap()
            .unwrap_or_else(|error| panic!("edit {index}: {error}"));
    }
}

struct Enrolled {
    device: SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    request_digest: [u8; 32],
    nonce: [u8; 32],
    reply: Vec<u8>,
}

/// The host's HTTPS local-owner completion: consume a real challenge, commit
/// the issuance, then build the reply exactly as `host::enrollment_reply`.
fn enroll(h: &mut Harness, seed: u8) -> Enrolled {
    let device = SigningKey::from_bytes(&[seed; 32]);
    let now = tilo_relay::clock::now_ns();
    let mut authority = crate::enrollment::EnrollmentAuthority::default();
    let challenge = authority
        .challenge(
            &h.f.owner,
            ChallengeRequest {
                device_public_key: hex::encode(device.verifying_key().to_bytes()),
                request_id: hex::encode([seed.wrapping_add(1); 32]),
            },
            now,
        )
        .unwrap();
    let challenge = decode(&challenge.challenge).unwrap();
    let complete = sign(&challenge, &device).unwrap();
    let request_digest = *blake3::hash(&postcard::to_stdvec(&complete).unwrap()).as_bytes();
    let (certificate, proof) =
        h.f.current
            .with_owner(&mut h.f.owner, |owner| {
                authority.complete_for_https(owner, complete, now)
            })
            .unwrap()
            .unwrap();
    let mut metadata = crate::enrollment::bootstrap_metadata(
        &h.f.owner,
        &certificate,
        serde_json::json!({"unused":true}),
        "http://127.0.0.1:1",
        now,
    )
    .unwrap();
    crate::enrollment::append_delivery_hint(&mut metadata, &h.f.owner, &h.ingress).unwrap();
    let reply = h
        .ingress
        .local_enrollment_reply(&h.f.owner, &certificate, proof, metadata)
        .unwrap();
    Enrolled {
        device,
        certificate,
        request_digest,
        nonce: challenge.nonce,
        reply,
    }
}

impl Harness {
    /// Fetch every signed range over TLS as the browser's handoff client does.
    async fn fetch_local_handoff(
        &mut self,
        host: &crate::host::LocalHost,
        enrolled: &Enrolled,
        manifest: &HandoffManifest<'_>,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        while bytes.len() < manifest.total_bytes as usize {
            let now = tilo_relay::clock::now_ns();
            let remaining = manifest.total_bytes as usize - bytes.len();
            let mut fetch = HandoffRequest {
                version: 1,
                manifest_digest: manifest.manifest_digest().unwrap(),
                device: enrolled.certificate.device_public_key,
                offset: bytes.len() as u64,
                length: remaining.min(MAX_HANDOFF_RANGE_BYTES) as u32,
                nonce: *blake3::hash(&(bytes.len() as u64).to_le_bytes()).as_bytes(),
                issued_at_ns: now,
                expires_at_ns: now + 20_000_000_000,
                signature: [[0; 32]; 2],
            };
            fetch.signature = split_signature(
                enrolled
                    .device
                    .sign(&fetch.signing_digest().unwrap())
                    .to_bytes(),
            );
            let transport = self.client.request_method(
                "POST",
                HANDOFF_CHUNKS_PATH,
                fetch.encode().unwrap(),
                None,
            );
            let ingress = &mut self.ingress;
            let owner = &mut self.f.owner;
            tokio::pin!(transport);
            let (status, encoded) = loop {
                tokio::select! {
                    out = &mut transport => break out,
                    incoming = ingress.recv() => {
                        let incoming = incoming.unwrap();
                        assert_eq!(incoming.route(), Route::OnboardingChunk);
                        ingress.onboard(owner, host, incoming).await;
                    }
                }
            };
            assert_eq!(
                status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&encoded)
            );
            let chunk = HandoffChunk::decode(&encoded).unwrap();
            chunk
                .verify_host_signature(manifest, &fetch, tilo_relay::clock::now_ns())
                .unwrap();
            assert_eq!(chunk.offset, bytes.len() as u64);
            bytes.extend_from_slice(chunk.body);
        }
        assert_eq!(blake3::hash(&bytes).as_bytes(), &manifest.content_digest);
        bytes
    }
}

#[tokio::test]
async fn local_owner_reenrollment_with_small_history_stays_inline() {
    let mut h = Harness::start().await;
    h.ingress
        .install_onboarding(crate::collaboration::CollaborationService::new(vec![]))
        .unwrap();
    let enrolled = enroll(&mut h, 201);
    let reply: serde_json::Value = serde_json::from_slice(&enrolled.reply).unwrap();
    let bootstrap = &reply["bootstrap"];
    assert!(bootstrap.get("governance_handoff").is_none());
    let messages: Vec<Vec<u8>> =
        serde_json::from_value(bootstrap["governance_messages"].clone()).unwrap();
    assert_eq!(
        messages,
        h.f.owner
            .outgoing()
            .iter()
            .map(|p| p.bytes.to_vec())
            .collect::<Vec<_>>(),
        "older runtimes keep reading the unchanged inline reply"
    );
    assert!(bootstrap.get("delivery_hint").is_some());
    h.ingress.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_owner_reenrollment_after_hundreds_of_publications_uses_chunked_handoff() {
    use qos_peer_state::{StatePeer, TransportOrigin};
    let mut h = Harness::start().await;
    let host = crate::host::LocalHost::start(&h.f.owner).await.unwrap();
    h.ingress
        .install_onboarding(crate::collaboration::CollaborationService::new(vec![]))
        .unwrap();
    publish_edits(&mut h, 250, 1536);
    let outgoing = h.f.owner.outgoing();
    let (count, total) = (
        outgoing.len(),
        outgoing.iter().map(|p| p.bytes.len()).sum::<usize>(),
    );
    drop(outgoing);
    // Owner reconnects repeatedly (reload, second tab, renewal) and each gets
    // its own bounded handoff; none needs a transport sync.
    for seed in [211u8, 221] {
        let enrolled = enroll(&mut h, seed);
        // The prior inline-only reply failed here: its JSON number arrays
        // exceeded the 896 KiB allowance and every reconnect answered 503.
        assert!(
            crate::enrollment::public_bootstrap(
                &h.f.owner,
                &enrolled.certificate,
                serde_json::Value::Null,
                "http://127.0.0.1:1",
                tilo_relay::clock::now_ns(),
            )
            .is_err_and(|error| error.status == 503),
            "{count} publications / {total} bytes must exceed the old inline bound"
        );
        assert!(enrolled.reply.len() <= 1024 * 1024);
        let reply: serde_json::Value = serde_json::from_slice(&enrolled.reply).unwrap();
        let bootstrap = &reply["bootstrap"];
        assert!(bootstrap.get("governance_messages").is_none());
        assert!(bootstrap.get("delivery_hint").is_some());
        assert_eq!(
            bootstrap["governance_generation"],
            h.f.owner.committed_generation().to_string()
        );
        let manifest_bytes: Vec<u8> =
            serde_json::from_value(bootstrap["governance_handoff"].clone()).unwrap();
        let manifest = HandoffManifest::decode(&manifest_bytes).unwrap();
        assert_eq!(manifest.purpose, HandoffPurpose::LocalGovernance);
        manifest
            .verify_host_signature(
                OnboardingScope {
                    origin: &h.ingress.origin,
                    host: h.f.owner.host_certificate().device_public_key,
                    workspace: h.f.owner.workspace_id(),
                    root: h.f.owner.peer().authority().user_public_key,
                    room: h.f.owner.peer().room().parse().unwrap(),
                },
                enrolled.certificate.device_public_key,
                enrolled.request_digest,
                enrolled.nonce,
                tilo_relay::clock::now_ns(),
            )
            .unwrap();
        assert!(manifest.total_bytes as usize > MAX_HANDOFF_RANGE_BYTES);
        let content = h.fetch_local_handoff(&host, &enrolled, &manifest).await;
        let bundle = GovernanceBundle::decode(&content).unwrap();
        // Enrollment itself commits (possibly replacing the baseline), so the
        // handoff is the owner's current recovery basis, item for item.
        assert_eq!(bundle.items.len(), h.f.owner.outgoing().len());
        for (item, original) in bundle.items.iter().zip(h.f.owner.outgoing().iter()) {
            assert_eq!(item.bytes, original.bytes);
            assert_eq!(item.retention, original.kind);
        }
        // A fresh browser state admits the handed-off history directly.
        let roster = serde_json::from_value(bootstrap["certified_roster"].clone()).unwrap();
        let mut fresh = StatePeer::new(
            h.f.owner.peer().room(),
            h.f.owner.peer().authority().clone(),
            roster,
            tilo_user_identity::RevocationList::new(),
            web_time::SystemTime::now(),
        )
        .unwrap();
        for item in &bundle.items {
            let origin =
                TransportOrigin::RelayDelta(h.f.owner.peer().authority().device_public_key);
            let outcome = fresh
                .receive_from(
                    origin,
                    postcard::from_bytes(item.bytes).unwrap(),
                    web_time::SystemTime::now(),
                )
                .unwrap();
            assert_eq!(outcome.rejected, 0);
        }
        assert_eq!(fresh.rows(), h.f.owner.peer().rows());
        assert!(
            fresh
                .permissions(
                    enrolled.certificate.device_public_key,
                    web_time::SystemTime::now()
                )
                .is_ok()
        );
    }
    h.ingress.shutdown().await.unwrap();
}
