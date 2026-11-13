//! Rewrite the single-file owner record fixture as this build writes it.
//!
//! `t cargo build -p tilo-peer-state --example regenerate_owner_fixture --features test-support`
//! and then run `./target/debug/examples/regenerate_owner_fixture` in the repository root.

use ed25519_dalek::SigningKey;
use std::time::{Duration, SystemTime};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::owner::{open_file, read_file, testing};

const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";
const RESEARCH: &str = "7fdaf11b-68aa-4e34-b01d-4ee2424fadcf";

fn main() -> std::process::ExitCode {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000);
    let key = SigningKey::from_bytes(&[12; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[11; 32]), &key, "A's desktop");
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("state.owner.json");
    futures::executor::block_on(async {
        let owner = open_file(&path).unwrap();
        for (id, name) in [(FOUNDERS, "Founders"), (RESEARCH, "Research")] {
            let workspace = WorkspaceId::parse(id).unwrap();
            owner
                .create_workspace(
                    workspace,
                    name,
                    &device,
                    &key,
                    Some(&testing::own_host_hint_here(workspace, &device, &key)),
                    now,
                )
                .await
                .unwrap();
        }
        let (intent, ..) = testing::raw_fixture_at(now);
        owner.prepare_enrollment_into(&intent, None, now).unwrap();
    });
    let record = read_file(&path).unwrap();
    println!("revision {}", record.revision());
    let bytes = serde_json::to_vec(&record).unwrap();
    let mut hex = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index % 16 == 0 && index > 0 {
            hex.push('\n');
        } else if index > 0 {
            hex.push(' ');
        }
        hex.push_str(&format!("{byte:02x}"));
    }
    hex.push('\n');
    let fixture = std::path::Path::new("crates/collab/tilo-peer-state")
        .join("tests/fixtures/owner-record/single-file.owner.json.hex");
    std::fs::write(&fixture, hex).unwrap();
    println!("{} bytes written to {}", bytes.len(), fixture.display());
    std::process::ExitCode::SUCCESS
}
