//! Requires a real installed original-copy record and actually completed owned
//! Btrfs shrink. Neither missing fixture nor unsuccessful observation is skipped.
use super::*;
use crate::setup::home_capture::SourceReadContext;
use std::{io::Read, os::unix::fs::MetadataExt, path::PathBuf};

#[test]
fn installed_completed_btrfs_copy_preserves_original_witness_and_inventory() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "root fixture required"
    );
    let system = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_COMPLETED_BTRFS_INSTALLED_ROOT")
            .expect("actual installed completed-Btrfs fixture required"),
    );
    let operation = std::env::var("TILO_ORIGINAL_COMPLETED_BTRFS_OPERATION")
        .expect("actual installed copy operation required");
    let installed = AuthenticatedInstalledOriginalCopy::open_fixture(&system, &operation).unwrap();
    let declaration = installed
        .manifest()
        .installed_original_copy_declaration()
        .unwrap()
        .unwrap();
    let requested_bytes = declaration.source().bytes().unwrap();
    let witness = declaration.source_witness().clone();
    assert!(installed.manifest().planned_btrfs_resize.is_some());
    let context = SourceReadContext::review_installed_copy(&installed).unwrap();
    assert_eq!(context.plan().bytes().unwrap(), requested_bytes);
    assert_eq!(context.intent.witness().unwrap(), witness);
    assert_ne!(
        context.source.original_plan().home.volume.last_lba,
        context.plan().home.volume.last_lba
    );
    let home = context.clone_original_home().unwrap();
    assert_eq!(home.metadata().unwrap().ino(), context.plan().home_inode());
    let inventory = context
        .original_flatpak_inventory()
        .unwrap()
        .expect("real protected selected-app inventory required");
    let (mut file, len, digest) = inventory;
    let mut before = Vec::new();
    file.read_to_end(&mut before).unwrap();
    assert_eq!(before.len() as u64, len);
    drop(file);
    let grant =
        crate::setup::flatpak_transfer::offline_original::OriginalInventoryGrant::review(&context)
            .unwrap()
            .expect("actual inventory grant required");
    context.original_flatpak_runner(&grant).unwrap();
    context.revalidate().unwrap();
    let (mut file, again_len, again_digest) =
        context.original_flatpak_inventory().unwrap().unwrap();
    let mut after = Vec::new();
    file.read_to_end(&mut after).unwrap();
    assert_eq!((again_len, again_digest), (len, digest));
    assert_eq!(after, before);
    drop(file);
    // The generic path is still strict and must not accept the old record as
    // though its requested geometry equalled a shrunk physical source.
    assert!(SourceReadContext::review_witness(context.source.clone(), &witness).is_err());
    let geometry = context.geometry.as_ref().unwrap();
    let mut wrong = geometry.effective.clone();
    wrong.home.volume.last_lba += 1;
    let forged = CompletedOriginalGeometry {
        installed: installed.clone(),
        original: geometry.original.clone(),
        shared: geometry.shared.clone(),
        resize: geometry.resize.clone(),
        effective: wrong,
    };
    assert!(forged.check(&context.source).is_err());
    let mut wrong = geometry.original.clone();
    wrong.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(geometry.map_related(&wrong).is_err());
    context.revalidate().unwrap();
    drop((home, grant));
    context.retire().unwrap();
    installed.revalidate().unwrap();
}
