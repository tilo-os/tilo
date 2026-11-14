//! Write the fixture of a workspace's file in the layout this build writes,
//! `current-layout.workspace.json.hex`: the released single-file record's
//! `Founders`, as this build brings it up and keeps it.
//!
//! Only when the layout gets a new version, with the step that reads the one
//! before. The released record (`single-file.owner.json.hex`) is what devices
//! hold and is never written again: this reads it.
//!
//! `t run -p tilo-peer-state --example regenerate_owner_fixture --features test-support`
//! in the repository root.

use std::error::Error;
use std::fmt::Write as _;
use std::path::Path;
use tilo_peer_state::owner::open_file;

const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";

fn main() -> Result<std::process::ExitCode, Box<dyn Error>> {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    let fixtures = Path::new("crates/collab/tilo-peer-state/tests/fixtures/owner-record");
    let released = std::fs::read_to_string(fixtures.join("single-file.owner.json.hex"))?
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16))
        .collect::<Result<Vec<u8>, _>>()?;
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
    let path = directory.path().join("state.owner.json");
    std::fs::write(&path, released)?;
    drop(open_file(&path)?);
    // The record is kept split beside its file, each workspace's file named
    // with the revision it was written at (the released record's, 3).
    let bytes = std::fs::read(
        path.with_extension("workspaces")
            .join(format!("{FOUNDERS}.3.json")),
    )?;
    let mut hex = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index % 16 == 0 && index > 0 {
            hex.push('\n');
        } else if index > 0 {
            hex.push(' ');
        }
        write!(hex, "{byte:02x}")?;
    }
    hex.push('\n');
    let fixture = fixtures.join("current-layout.workspace.json.hex");
    std::fs::write(&fixture, hex)?;
    println!("{} bytes written to {}", bytes.len(), fixture.display());
    Ok(std::process::ExitCode::SUCCESS)
}
