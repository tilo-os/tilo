use super::*;
pub(crate) use imager_backend::test_support::media::{IMAGE_BYTES, MEDIA_BYTES, disk};
use std::{os::unix::fs::FileExt, process::Command};
use tilo_test_env::SpawnUnderEnvLock as _;
tilo_test_env::scenario! {
    fn a_media_layout_changes_only_what_its_approved_plan_names;

    phase production_provision_preserves_payload_and_matches_native_partition_reader;
    {
        let (_dir, path, file) = disk();
        let mut original = vec![0; 128];
        file.read_exact_at(&mut original, 1024)
            .expect("original entry");
        let result =
            provision(&file, IMAGE_BYTES, MEDIA_BYTES, "usb:test", &|| false).expect("provision");
        assert_eq!(result.number, 2);
        assert!(result.first_lba * 512 >= IMAGE_BYTES);
        let mut kept = vec![0; 128];
        file.read_exact_at(&mut kept, 1024).expect("retained entry");
        assert_eq!(kept, original);
        let mut payload = vec![0; 19];
        file.read_exact_at(&mut payload, 2048 * 512)
            .expect("payload");
        assert_eq!(payload, b"signed boot payload");
        assert_eq!(
            binding(&file, MEDIA_BYTES, "usb:test").expect("independent binding"),
            result
        );
        // Read the on-disk GPT through util-linux, independently of the writer's
        // own decoder. The profile occupies the reserved gap after LastUsableLBA.
        let output = Command::new("sfdisk")
            .arg("--json")
            .arg(path)
            .output_locked()
            .expect("native partition reader (util-linux sfdisk)");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let readback: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let table = &readback["partitiontable"];
        let sector_bytes = table["sectorsize"].as_u64().expect("sector size");
        assert_eq!(
            (table["lastlba"].as_u64().expect("last usable LBA") + 1) * sector_bytes,
            result.profile_offset
        );
        let partition = &table["partitions"][1];
        assert_eq!(partition["start"].as_u64(), Some(result.first_lba));
        assert_eq!(
            partition["size"].as_u64(),
            Some(result.last_lba - result.first_lba + 1)
        );
        // GUIDs compare without case: sfdisk prints them in capitals.
        assert_eq!(
            partition["uuid"].as_str().map(str::to_ascii_lowercase),
            Some(result.partition_guid.to_ascii_lowercase())
        );
    }

    phase invalid_or_cancelled_media_never_rewrites_metadata;
    {
        let (_dir, _path, file) = disk();
        let mut original = vec![0; 32768];
        file.read_exact_at(&mut original, 0).expect("original");
        assert!(provision(&file, 0, MEDIA_BYTES, "usb:test", &|| false).is_err());
        assert!(provision(&file, IMAGE_BYTES, MEDIA_BYTES, "usb:test", &|| true).is_err());
        assert!(provision(&file, IMAGE_BYTES, MEDIA_BYTES - 512, "usb:test", &|| false).is_err());
        let mut actual = vec![0; 32768];
        file.read_exact_at(&mut actual, 0).expect("unchanged");
        assert_eq!(actual, original);
        file.write_all_at(&[1], 512 + 16).expect("corrupt CRC");
        assert!(provision(&file, IMAGE_BYTES, MEDIA_BYTES, "usb:test", &|| false).is_err());
    }

    phase retained_approved_plan_repairs_only_its_durable_backup_transaction;
    {
        let (_dir, _path, file) = disk();
        let plan =
            gpt::Layout::plan(&file, IMAGE_BYTES, MEDIA_BYTES, "usb:test").expect("approved plan");
        plan.stage_backup(&file).expect("durable backup");
        file.write_all_at(&[17], 1024 + 128)
            .expect("interrupted primary array write");
        assert!(binding(&file, MEDIA_BYTES, "usb:test").is_err());
        plan.commit(&file, &|| false)
            .expect("repair own transaction");
        assert_eq!(
            binding(&file, MEDIA_BYTES, "usb:test")
                .expect("consistent GPT")
                .number,
            2
        );
        let (_other_dir, _other_path, other) = disk();
        let other_plan =
            gpt::Layout::plan(&other, IMAGE_BYTES, MEDIA_BYTES, "usb:test").expect("other plan");
        other_plan.stage_backup(&other).expect("durable backup");
        other
            .write_all_at(&[17], 1024 + 128)
            .expect("interrupted primary");
        other
            .write_all_at(&[17], MEDIA_BYTES - 512 + 16)
            .expect("foreign damaged backup");
        assert!(other_plan.commit(&other, &|| false).is_err());
    }
}
#[cfg(feature = "installation")]
#[test]
fn pending_plan_recomputes_geometry_and_capacity_before_any_mutation() {
    let (_root, _path, target) = disk();
    let media_bytes = 128 * 1024 * 1024;
    target
        .set_len(media_bytes)
        .expect("bounded fixed-policy medium");
    let original = std::fs::read(&_path).expect("original media");
    let planned = prepare(&target, &target, IMAGE_BYTES, media_bytes, "usb:pending")
        .expect("held descriptor planning");
    let expected = planned.planned_binding().expect("canonical future binding");
    let payload_capacity = planned
        .payload_capacity()
        .expect("actual canonical payload capacity");
    planned.require_capacity(payload_capacity).unwrap();
    assert!(
        planned
            .require_capacity(payload_capacity.checked_add(1).unwrap())
            .is_err()
    );
    planned
        .require_capacity(1024 * 1024)
        .expect("bounded reservation");
    assert_eq!(std::fs::read(&_path).expect("unwritten media"), original);
    let raw_extent = (expected.last_lba - expected.first_lba + 1) * expected.sector_bytes;
    let near_raw = raw_extent - 1;
    assert!(near_raw < raw_extent); // The old raw-extent admission accepted this.
    assert!(
        prepare_expected(
            &target,
            &target,
            IMAGE_BYTES,
            media_bytes,
            &expected,
            near_raw
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(&_path).expect("formatted-capacity refusal preserves all bytes"),
        original
    );
    let mut changed = expected.clone();
    changed.first_lba += 1;
    assert!(
        prepare_expected(
            &target,
            &target,
            IMAGE_BYTES,
            media_bytes,
            &changed,
            1024 * 1024
        )
        .is_err()
    );
    assert!(
        prepare_expected(
            &target,
            &target,
            IMAGE_BYTES,
            media_bytes,
            &expected,
            media_bytes
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(&_path).expect("refusal preserves bytes"),
        original
    );
    let verified = prepare_expected(
        &target,
        &target,
        IMAGE_BYTES,
        media_bytes,
        &expected,
        1024 * 1024,
    )
    .expect("privileged independent recomputation");
    assert_eq!(
        verified
            .commit(&target, &|| false)
            .expect("actual GPT commit"),
        expected
    );
    assert_eq!(
        binding(&target, media_bytes, "usb:pending").expect("readback"),
        expected
    );
}
