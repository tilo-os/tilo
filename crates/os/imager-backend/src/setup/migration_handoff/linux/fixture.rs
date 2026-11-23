//! Synthetic topology used only to exercise plan validation, never admission.
use super::*;

pub(super) fn facts(base: &Path, read_only: bool) -> Facts {
    let sample = super::super::fixture::sample();
    let source = &sample.home.volume;
    let device = BlockDevice {
        name: "/dev/testdisk1".into(),
        kind: "part".into(),
        size: source
            .last_lba
            .checked_sub(source.first_lba)
            .unwrap()
            .checked_add(1)
            .unwrap()
            .checked_mul(512)
            .unwrap(),
        start: Some(source.first_lba),
        fstype: Some("ext4".into()),
        uuid: Some(source.filesystem_uuid.clone()),
        partuuid: Some(source.partition_guid.clone()),
        maj_min: Some("8:1".into()),
        ..Default::default()
    };
    Facts {
        owned_bindings: Vec::new(),
        mounts: vec![Mount {
            id: 99,
            filesystem: "ext4".into(),
            private: true,
            number: "8:1".into(),
            root: "/".into(),
            point: base.into(),
            source: "/dev/testdisk1".into(),
            read_only,
            source_eligible: true,
        }],
        devices: vec![BlockDevice {
            name: "/dev/testdisk".into(),
            kind: "disk".into(),
            size: source.disk_bytes,
            pttype: Some("gpt".into()),
            ptuuid: Some(source.disk_guid.clone()),
            log_sec: Some(512),
            children: vec![device],
            ..Default::default()
        }],
    }
}
