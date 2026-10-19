//! Bounded signed join endpoint for the isolated packaged-desktop proof.
//! Uses the scratch session's enrolled custody; creates no app records or
//! agent authority. The desktop must redeem the invitation normally.

use std::{
    net::Ipv4Addr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_delivery_protocol::workspace_join::{WORKSPACE_JOIN_ALPN, WorkspaceJoinService};
use tilo_joined_room::testing::governor::TestGovernor;
use tilo_peer_state::join::{JoinTransportRequest, JoinTransportResponse, MemberRole};
use tilo_relay::workspace_join::WorkspaceJoinProtocol;

#[derive(Clone)]
struct FixtureService(Arc<Mutex<TestGovernor>>);

impl std::fmt::Debug for FixtureService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PackagedNativeGovernor")
    }
}

impl WorkspaceJoinService for FixtureService {
    async fn handle(
        &self,
        remote: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        match self.0.lock() {
            Ok(mut governor) => governor.handle(remote, request),
            Err(_) => JoinTransportResponse::Error {
                code: 503,
                message: "fixture custody unavailable".into(),
            },
        }
    }
}

fn fixture_output() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let profile = PathBuf::from(args.next().ok_or("expected scratch profile")?).canonicalize()?;
    let output = PathBuf::from(args.next().ok_or("expected invitation path")?);
    if args.next().is_some()
        || !output.is_absolute()
        || output.exists()
        || output.parent() != Some(profile.as_path())
        || runtime_identity::identity_dir_from_env()? != profile.join("config/tilo/identity")
        || std::env::var_os("TILO_IDENTITY_ROOT").map(PathBuf::from)
            != Some(profile.join("identity"))
    {
        return Err(
            "fixture must use its own provisioned scratch profile and new private output".into(),
        );
    }
    Ok(output)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let output = fixture_output()?;
    tilo_identity_host::provisioning::ensure_throwaway_test_identity(
        &runtime_identity::identity_dir_from_env()?,
        &tilo_identity_host::provisioning::collaboration_identity_paths(),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )?;
    let room = uuid::Uuid::new_v4().to_string();
    let (key, certificate) = tilo_identity_host::broker::trusted_current_device(&room)?;
    let mut governor = TestGovernor::empty_interactive(certificate.clone(), key, &room)?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))?
        .bind()
        .await?;
    let hint = governor.hint(endpoint.addr());
    let expires = tilo_clock::try_unix_now_ns()
        .ok_or("fixture clock is before the Unix epoch")?
        .checked_add(120_000_000_000)
        .ok_or("fixture expiry overflow")?;
    let signed = governor.issue(
        vec![hint],
        MemberRole::Editor,
        Some(certificate.user_public_key),
        Some("Packaged desktop proof".into()),
        expires,
    );
    let router = iroh::protocol::Router::builder(endpoint)
        .accept(
            WORKSPACE_JOIN_ALPN,
            WorkspaceJoinProtocol::new(FixtureService(Arc::new(Mutex::new(governor)))),
        )
        .spawn();
    // This capability stays in the owned private scratch profile, never in
    // logs, screenshots or native-floating diagnostics.
    let bytes = serde_json::to_vec(&serde_json::json!({
        "room": room, "invitation": tilo_peer_state::join::encode_invitation_text(&signed),
        "empty_app_records": true,
    }))?;
    if !tilo_files::native::private::create_atomic(&output, &bytes)? {
        return Err("fixture invitation output already exists".into());
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
    router.shutdown().await?;
    Ok(())
}
