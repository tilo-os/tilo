//! Filesystem barriers, contention and restart refusal on the actual private journal.
use super::journal::Journal;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

type Check = Result<(), Box<dyn std::error::Error>>;
fn private_root() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    Ok(root)
}
#[test]
fn committed_launch_barrier_survives_owner_restart_and_cannot_be_reused() -> Check {
    let root = private_root()?;
    let slot = "a".repeat(64);
    let journal = Journal::open(root.path())?;
    journal.claim(&slot, br#"{"observed":"claimed"}"#)?;
    journal.process(&slot, br#"{"observed":"bound"}"#)?;
    if Journal::open(root.path()).is_ok() {
        return Err("a second journal owner acquired live custody".into());
    }
    drop(journal);
    let reopened = Journal::open(root.path())?;
    if reopened.claim(&slot, b"replacement").is_ok()
        || fs::read(root.path().join(format!("{slot}.claim.json")))? != br#"{"observed":"claimed"}"#
    {
        return Err("reopening retried or overwrote an existing launch claim".into());
    }
    drop(reopened);
    Ok(())
}
#[test]
fn partial_claim_and_symlinked_process_evidence_never_authorize_replacement() -> Check {
    let root = private_root()?;
    let outside = root.path().join("outside");
    let slot = "b".repeat(64);
    fs::write(&outside, b"original")?;
    fs::write(root.path().join(format!("{slot}.claim.json")), b"")?;
    symlink(&outside, root.path().join(format!("{slot}.process.json")))?;
    let journal = Journal::open(root.path())?;
    if journal.claim(&slot, b"replacement").is_ok()
        || journal.process(&slot, b"replacement").is_ok()
        || fs::read(&outside)? != b"original"
        || fs::metadata(root.path().join(format!("{slot}.claim.json")))?.len() != 0
    {
        return Err("uncertain or redirected evidence was overwritten".into());
    }
    drop(journal);
    Ok(())
}
#[test]
fn public_directory_and_oversized_evidence_are_refused_before_writing() -> Check {
    let root = private_root()?;
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755))?;
    if Journal::open(root.path()).is_ok() {
        return Err("public journal directory admitted".into());
    }
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
    let journal = Journal::open(root.path())?;
    let slot = "c".repeat(64);
    if journal.claim(&slot, &vec![0; 16 * 1024 + 1]).is_ok()
        || root.path().join(format!("{slot}.claim.json")).exists()
    {
        return Err("oversized claim created a launch barrier".into());
    }
    drop(journal);
    Ok(())
}
