//! An in-memory platform for the staging helper's tests: a sparse disk, crash
//! points and a log of every mutation.

use std::collections::{BTreeMap, HashMap};

use imager_backend::platform::{Disk, IoResult, NewPartition, Partition, Platform, SystemVolume};

const BLOCK: u64 = 4096;

#[derive(Clone, Debug, Default)]
pub(crate) struct Faults {
    pub crash_after: Option<usize>,
    pub fail_at: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct FakePlatform {
    pub disk: Disk,
    pub volume: Option<SystemVolume>,
    pub vars: BTreeMap<String, Vec<u8>>,
    pub firmware_available: bool,
    blocks: HashMap<u64, Vec<u8>>,
    pub mutations: usize,
    pub faults: Faults,
    pub log: Vec<String>,
    /// Files the helper placed inside the staging root, by partition and path.
    pub files: HashMap<(String, String), Vec<u8>>,
}

impl FakePlatform {
    pub(crate) fn new(disk: Disk, volume: Option<SystemVolume>) -> Self {
        let mut vars = BTreeMap::new();
        vars.insert("BootCurrent".into(), vec![1, 0]);
        vars.insert("BootOrder".into(), vec![1, 0, 0, 0]);
        vars.insert("Boot0000".into(), b"firmware-shell".to_vec());
        vars.insert("Boot0001".into(), b"retained-os".to_vec());
        vars.insert("SecureBoot".into(), vec![0]);
        Self {
            disk,
            volume,
            vars,
            firmware_available: true,
            blocks: HashMap::new(),
            mutations: 0,
            faults: Faults::default(),
            log: Vec::new(),
            files: HashMap::new(),
        }
    }

    fn before(&mut self, what: String) -> IoResult<()> {
        let next = self.mutations + 1;
        if self.faults.fail_at == Some(next) {
            self.faults.fail_at = None;
            return Err(format!("injected failure: {what}"));
        }
        self.log.push(what);
        Ok(())
    }

    #[allow(
        clippy::panic,
        reason = "the fake's injected crash is the behaviour under test"
    )]
    fn after(&mut self) {
        self.mutations += 1;
        if self.faults.crash_after == Some(self.mutations) {
            self.faults.crash_after = None;
            panic!("injected crash after mutation {}", self.mutations);
        }
    }

    fn partition(&self, guid: &str) -> IoResult<Partition> {
        self.disk
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, guid))
            .cloned()
            .ok_or_else(|| format!("no partition {guid}"))
    }

    fn span(&self, disk: &Disk, guid: &str, offset: u64, len: usize) -> IoResult<u64> {
        if disk.guid != self.disk.guid {
            return Err("wrong disk".into());
        }
        let p = self.partition(guid)?;
        let s = self.disk.sector_bytes;
        if !offset.is_multiple_of(s) || !(len as u64).is_multiple_of(s) {
            return Err("unaligned partition I/O".into());
        }
        let size = (p.last_lba - p.first_lba + 1) * s;
        if offset + len as u64 > size {
            return Err("I/O beyond the partition".into());
        }
        Ok(p.first_lba * s + offset)
    }

    pub(crate) fn read_abs(&self, at: u64, buf: &mut [u8]) {
        let mut done = 0;
        while done < buf.len() {
            let pos = at + done as u64;
            let within = (pos % BLOCK) as usize;
            let n = (BLOCK as usize - within).min(buf.len() - done);
            match self.blocks.get(&(pos / BLOCK)) {
                Some(blk) => buf[done..done + n].copy_from_slice(&blk[within..within + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
    }

    fn write_abs(&mut self, at: u64, data: &[u8]) {
        let mut done = 0;
        while done < data.len() {
            let pos = at + done as u64;
            let within = (pos % BLOCK) as usize;
            let n = (BLOCK as usize - within).min(data.len() - done);
            let piece = &data[done..done + n];
            let key = pos / BLOCK;
            if piece.iter().all(|b| *b == 0) && !self.blocks.contains_key(&key) {
                done += n;
                continue;
            }
            let blk = self
                .blocks
                .entry(key)
                .or_insert_with(|| vec![0; BLOCK as usize]);
            blk[within..within + n].copy_from_slice(piece);
            done += n;
        }
    }

    pub(crate) fn partition_bytes(&self, guid: &str, len: usize) -> Vec<u8> {
        let p = self.partition(guid).unwrap();
        let mut buf = vec![0; len];
        self.read_abs(p.first_lba * self.disk.sector_bytes, &mut buf);
        buf
    }

    pub(crate) fn tamper(&mut self, guid: &str, offset: u64, data: &[u8]) {
        let p = self.partition(guid).unwrap();
        self.write_abs(p.first_lba * self.disk.sector_bytes + offset, data);
    }
}

impl Platform for FakePlatform {
    fn has_uefi(&self) -> bool {
        self.firmware_available
    }

    fn system_disk(&self) -> IoResult<Disk> {
        let mut disk = self.disk.clone();
        disk.partitions.sort_by_key(|p| p.first_lba);
        Ok(disk)
    }

    fn system_volume(&self) -> IoResult<Option<SystemVolume>> {
        Ok(self.volume.clone())
    }

    fn shrink_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()> {
        self.resize(partition_guid, to_bytes, true)
    }

    fn grow_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()> {
        self.resize(partition_guid, to_bytes, false)
    }

    fn create_partition(&mut self, disk: &Disk, spec: &NewPartition) -> IoResult<String> {
        if disk.guid != self.disk.guid {
            return Err("wrong disk".into());
        }
        if self
            .disk
            .partitions
            .iter()
            .any(|p| p.first_lba <= spec.last_lba && spec.first_lba <= p.last_lba)
        {
            return Err("overlapping partition".into());
        }
        self.before(format!("create {}..{}", spec.first_lba, spec.last_lba))?;
        let guid = spec.guid.clone().unwrap_or_else(super::random_guid);
        let number = (1..=u32::MAX)
            .find(|n| !self.disk.partitions.iter().any(|p| p.number == *n))
            .expect("free number");
        self.disk.partitions.push(Partition {
            number,
            guid: guid.clone(),
            type_guid: spec.type_guid.clone(),
            first_lba: spec.first_lba,
            last_lba: spec.last_lba,
        });
        self.after();
        Ok(guid)
    }

    fn delete_partition(&mut self, disk: &Disk, partition_guid: &str) -> IoResult<()> {
        if disk.guid != self.disk.guid {
            return Err("wrong disk".into());
        }
        self.partition(partition_guid)?;
        self.before(format!("delete {partition_guid}"))?;
        self.disk
            .partitions
            .retain(|p| !super::same_guid(&p.guid, partition_guid));
        self.after();
        Ok(())
    }

    fn write_partition(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        data: &[u8],
    ) -> IoResult<()> {
        let at = self.span(disk, partition_guid, offset, data.len())?;
        self.before(format!("write {partition_guid} @{offset}+{}", data.len()))?;
        self.write_abs(at, data);
        self.after();
        Ok(())
    }

    fn read_partition(
        &self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> IoResult<()> {
        let at = self.span(disk, partition_guid, offset, buf.len())?;
        self.read_abs(at, buf);
        Ok(())
    }

    fn write_staging_file(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        contents: &[u8],
        _mode: u32,
    ) -> IoResult<()> {
        if disk.guid != self.disk.guid {
            return Err("wrong disk".into());
        }
        self.partition(partition_guid)?;
        self.before(format!("file {partition_guid}:{path}"))?;
        self.files.insert(
            (partition_guid.to_ascii_lowercase(), path.to_owned()),
            contents.to_vec(),
        );
        self.after();
        Ok(())
    }

    fn read_staging_file(
        &self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
    ) -> IoResult<Option<Vec<u8>>> {
        if disk.guid != self.disk.guid {
            return Err("wrong disk".into());
        }
        Ok(self
            .files
            .get(&(partition_guid.to_ascii_lowercase(), path.to_owned()))
            .cloned())
    }

    fn write_staging_file_from(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        source: &std::path::Path,
        mode: u32,
    ) -> IoResult<u64> {
        let contents = std::fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
        self.write_staging_file(disk, partition_guid, path, &contents, mode)?;
        Ok(contents.len() as u64)
    }

    fn staging_file_matches(
        &self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        source: &std::path::Path,
    ) -> IoResult<bool> {
        let contents = std::fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
        Ok(self
            .read_staging_file(disk, partition_guid, path)?
            .as_deref()
            == Some(&contents[..]))
    }

    fn sync_disk(&mut self, _disk: &Disk) -> IoResult<()> {
        Ok(())
    }

    fn get_efi_var(&self, name: &str) -> IoResult<Option<Vec<u8>>> {
        Ok(self.vars.get(name).cloned())
    }

    fn set_efi_var(&mut self, name: &str, data: &[u8]) -> IoResult<()> {
        self.before(format!("set {name}"))?;
        self.vars.insert(name.into(), data.to_vec());
        self.after();
        Ok(())
    }

    fn delete_efi_var(&mut self, name: &str) -> IoResult<()> {
        self.before(format!("delete-var {name}"))?;
        self.vars.remove(name);
        self.after();
        Ok(())
    }
}

impl FakePlatform {
    fn resize(&mut self, guid: &str, to_bytes: u64, shrink: bool) -> IoResult<()> {
        let volume = self.volume.clone().ok_or("no system volume")?;
        if !super::same_guid(&volume.partition_guid, guid) {
            return Err("not the system volume".into());
        }
        if shrink == (to_bytes >= volume.size_bytes) {
            return Err("resize in the wrong direction".into());
        }
        let s = self.disk.sector_bytes;
        let c = self.partition(guid)?;
        let last = c.first_lba + to_bytes / s - 1;
        if !shrink
            && self
                .disk
                .partitions
                .iter()
                .any(|p| p.first_lba > c.last_lba && p.first_lba <= last)
        {
            return Err("no room to grow".into());
        }
        self.before(format!("resize {guid} -> {to_bytes}"))?;
        for p in &mut self.disk.partitions {
            if super::same_guid(&p.guid, guid) {
                p.last_lba = last;
            }
        }
        if let Some(v) = self.volume.as_mut() {
            v.size_bytes = to_bytes;
        }
        self.after();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::handoff::Handoff;
    use super::super::helper::{
        self, Action, ComponentPaths, Refusal, ReleaseAuthority, Request, codes,
    };
    use super::super::plan::{self, Basis, Plan, Requirements};
    use super::super::record::{RecordStatus, RecordStore, ReleaseRef, Step, StepState};
    use super::*;
    use crate::setup::internal_bundle::{BundleFacts, ComponentFacts};
    use imager_backend::{efi, staging::fat};
    use sha2::{Digest, Sha256};

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    struct TestAuthority(BundleFacts);

    impl ReleaseAuthority for TestAuthority {
        fn verify(&self, release: &ReleaseRef, _manifest: &[u8]) -> Result<BundleFacts, String> {
            if release.release_id == self.0.release_id {
                Ok(self.0.clone())
            } else {
                Err("unknown release".into())
            }
        }
    }

    struct World {
        _dir: tempfile::TempDir,
        store: RecordStore,
        authority: TestAuthority,
        request: Request,
        esp: Vec<u8>,
        root: Vec<u8>,
        root_path: std::path::PathBuf,
    }

    fn sha(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    fn linux_platform() -> FakePlatform {
        let basis = plan::tests::linux_basis(
            512,
            64 * GIB,
            vec![plan::tests::part(
                1,
                "a0000000-0000-4000-8000-000000000001",
                2048,
                2048 + 600 * MIB / 512 - 1,
            )],
        );
        FakePlatform::new(basis.disk, None)
    }

    fn linux_4kn_platform() -> FakePlatform {
        let basis = plan::tests::linux_basis(4096, 64 * GIB, Vec::new());
        FakePlatform::new(basis.disk, None)
    }

    fn windows_platform() -> FakePlatform {
        let basis = plan::tests::windows_basis(128, 100, 30, true);
        FakePlatform::new(basis.disk, basis.system_volume)
    }

    fn basis_of(p: &FakePlatform) -> Basis {
        helper::read_basis(p).unwrap()
    }

    const FIXTURE_ROOT_FSID: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
    const FIXTURE_ROOT_FSID_BYTES: [u8; 16] = [
        0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2, 0xe1,
        0xf0,
    ];

    fn world(platform: &FakePlatform) -> World {
        let sector = platform.disk.sector_bytes as usize;
        world_with_esp(
            platform,
            fat::tests::staging_esp_image_with_sector(false, 8192, sector),
        )
    }

    fn world_with_esp(platform: &FakePlatform, esp: Vec<u8>) -> World {
        let dir = tempfile::tempdir().unwrap();
        let root: Vec<u8> = (0..3 * 512_u32).map(|i| (i * 7 % 251) as u8).collect();
        let esp_path = dir.path().join("staging-esp.img");
        let root_path = dir.path().join("staging-root.img");
        let manifest_path = dir.path().join("manifest.json");
        std::fs::write(&esp_path, &esp).unwrap();
        std::fs::write(&root_path, &root).unwrap();
        let manifest = b"{\"fixture\":true}".to_vec();
        std::fs::write(&manifest_path, &manifest).unwrap();
        let facts = BundleFacts {
            release_id: "fixture-only".into(),
            architecture: "x86_64".into(),
            esp: ComponentFacts {
                byte_size: esp.len() as u64,
                sha256: sha(&esp),
                minimum_partition_bytes: 2 * MIB,
                filesystem_uuid: "1234-ABCD".into(),
            },
            root: ComponentFacts {
                byte_size: root.len() as u64,
                sha256: sha(&root),
                minimum_partition_bytes: 3 * MIB,
                filesystem_uuid: FIXTURE_ROOT_FSID.into(),
            },
            efi_path: "/EFI/BOOT/BOOTX64.EFI".into(),
        };
        let basis = basis_of(platform);
        let plan = plan::with_loader_path(
            plan::compute(&basis, facts.requirements(35 * GIB)).unwrap(),
            &facts.efi_path,
        );
        let mut profile = serde_json::Map::new();
        profile.insert("schema".into(), 1.into());
        profile.insert("hostname".into(), "tilo".into());
        let request = Request {
            schema: 1,
            operation_id: "0a0b0c0d-0000-4000-8000-000000000001".into(),
            action: Action::Stage,
            release: ReleaseRef {
                release_id: "fixture-only".into(),
                manifest_sha256: sha(&manifest),
                architecture: "x86_64".into(),
            },
            manifest_path: manifest_path.to_string_lossy().into(),
            component_paths: ComponentPaths {
                staging_esp: esp_path.to_string_lossy().into(),
                staging_root: root_path.to_string_lossy().into(),
            },
            plan_sha256: plan.digest().unwrap(),
            plan,
            profile,
            install_plan: None,
            token_file: None,
            development_layers: None,
        };
        World {
            store: RecordStore::at(dir.path()),
            _dir: dir,
            authority: TestAuthority(facts),
            request,
            esp,
            root,
            root_path,
        }
    }

    fn apply(p: &mut FakePlatform, w: &World, action: Action) -> Result<(), Refusal> {
        let mut request = w.request.clone();
        request.action = action;
        if action != Action::Stage {
            request.manifest_path = String::new();
            request.component_paths = ComponentPaths {
                staging_esp: String::new(),
                staging_root: String::new(),
            };
            request.profile = serde_json::Map::new();
        }
        let mut lines = Vec::new();
        let result = helper::execute(p, &w.store, &w.authority, &request, &mut |l| {
            lines.push(l.clone());
        });
        assert!(
            lines
                .iter()
                .all(|l| l.done_bytes <= l.total_bytes || l.total_bytes == 0)
        );
        result
    }

    fn staged_guids(p: &FakePlatform, plan: &Plan) -> (String, String) {
        let find = |first: u64| {
            p.disk
                .partitions
                .iter()
                .find(|x| x.first_lba == first)
                .map(|x| x.guid.clone())
                .expect("staged partition")
        };
        (
            find(plan.staging_esp.first_lba),
            find(plan.staging_root.first_lba),
        )
    }

    fn assert_staged(p: &FakePlatform, w: &World) {
        let plan = &w.request.plan;
        let (esp_guid, root_guid) = staged_guids(p, plan);
        if let Some(g) = &plan.staging_esp.partition_guid {
            assert_eq!(&esp_guid, g);
        }
        let esp_part = p.partition(&esp_guid).unwrap();
        assert_eq!(esp_part.type_guid, super::super::ESP_TYPE);
        let root_part = p.partition(&root_guid).unwrap();
        assert_eq!(root_part.type_guid, super::super::STAGING_ROOT_TYPE);
        assert_eq!(p.partition_bytes(&root_guid, w.root.len()), w.root);
        let mut on_disk = p.partition_bytes(&esp_guid, w.esp.len());
        let mut read = |at: u64, buf: &mut [u8]| {
            buf.copy_from_slice(&w.esp[at as usize..at as usize + buf.len()]);
            Ok(())
        };
        let handoff_ranges = fat::file_ranges(
            &mut read,
            w.esp.len() as u64,
            "/EFI/tilo-staging/handoff.json",
        )
        .unwrap();
        let record_ranges = fat::file_ranges(
            &mut read,
            w.esp.len() as u64,
            "/EFI/tilo-staging/record.json",
        )
        .unwrap();
        let gather = |img: &[u8], ranges: &[fat::Range]| -> Vec<u8> {
            ranges
                .iter()
                .flat_map(|r| img[r.offset as usize..(r.offset + r.len) as usize].to_vec())
                .collect()
        };
        let handoff = Handoff::parse(&gather(&on_disk, &handoff_ranges)).unwrap();
        assert_eq!(handoff.staging_esp.partuuid, esp_guid);
        assert_eq!(handoff.staging_root.partuuid, root_guid);
        assert_eq!(handoff.target_extent, plan.target_extent);
        assert_eq!(handoff.original_boot_order, vec!["0001", "0000"]);
        let copy: super::super::record::Record =
            serde_json::from_slice(&gather(&on_disk, &record_ranges)).unwrap();
        assert_eq!(copy.operation_id, w.request.operation_id);
        let mut all = handoff_ranges;
        all.extend(&record_ranges);
        fat::mask(&mut on_disk, 0, &all);
        assert_eq!(on_disk, w.esp, "the ESP is the image plus the handoff only");
        let number = efi::parse_boot_number(&handoff.owned_boot_entry).unwrap();
        assert_eq!(number, 0x0100);
        let option = efi::decode_load_option(&p.vars["Boot0100"]).unwrap();
        assert_eq!(option.partition_guid, esp_guid);
        assert_eq!(option.partition_number, esp_part.number);
        assert_eq!(option.start_lba, esp_part.first_lba);
        assert_eq!(option.file_path, "\\EFI\\BOOT\\BOOTX64.EFI");
        assert_eq!(p.vars["BootNext"], vec![0x00, 0x01]);
        assert_eq!(p.vars["BootOrder"], vec![1, 0, 0, 0]);
        let record = w.store.load(&w.request.operation_id).unwrap().unwrap();
        assert_eq!(record.status(), RecordStatus::Staged);
        assert_eq!(
            record.owned_boot_entry.as_ref().unwrap().load_option_sha256,
            sha(&p.vars["Boot0100"])
        );
        assert_eq!(record.original_firmware.boot_order, vec!["0001", "0000"]);
        assert_eq!(
            super::super::record::pending_in(w.store.public_dir.as_ref().unwrap()).len(),
            1,
            "the UI learns of the staged operation through the public mirror"
        );
    }

    fn server_plan(
        rule: install_profile_model::plan::DiskRule,
    ) -> install_profile_model::plan::Plan {
        let mut plan: install_profile_model::plan::Plan =
            serde_json::from_value(serde_json::json!({
                "schema": 1,
                "plan": "0123456789abcdef0123456789abcdef",
                "revision": 1,
                "destination": "tilo-server",
                "name": "cupboard",
                "network": {"addressing": "dhcp", "addresses": "", "gateway": "", "dns": ""},
                "disk": {"kind": "serial", "identity": "16Q0A07GTM7J"},
                "unlock": "owner-device",
                "updates": "stable",
                "hosting": [],
                "open": [],
                "token": {"nonce": "00112233445566778899aabbccddeeff",
                          "expires_at_ns": 1_700_000_000_000_000_000_u64}
            }))
            .unwrap();
        plan.disk = rule;
        plan
    }

    // The token is a file only its owner reads, which the test sets with Unix modes.
    #[cfg(unix)]
    #[test]
    fn a_staged_plan_and_its_token_are_given_together_and_name_their_disk() {
        let platform = linux_platform();
        let mut world = world(&platform);
        let rule = |kind: &str, identity: &str| install_profile_model::plan::DiskRule {
            kind: kind.to_owned(),
            identity: identity.to_owned(),
        };
        let token = world._dir.path().join("machine.token");
        std::fs::write(&token, b"token").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let token = token.to_string_lossy().into_owned();

        let refusal =
            |world: &mut World| helper::validate_request(&world.request).unwrap_err().code;

        world.request.install_plan = Some(server_plan(rule("serial", "16Q0A07GTM7J")));
        assert_eq!(
            refusal(&mut world),
            codes::REQUEST_INVALID,
            "a plan needs its token"
        );

        world.request.install_plan = None;
        world.request.token_file = Some(token);
        assert_eq!(
            refusal(&mut world),
            codes::REQUEST_INVALID,
            "a token needs its plan"
        );

        let mut desktop = server_plan(rule("serial", "16Q0A07GTM7J"));
        desktop.destination = "tilo-os".to_owned();
        world.request.install_plan = Some(desktop);
        assert_eq!(
            refusal(&mut world),
            codes::REQUEST_INVALID,
            "only a server installs without a person"
        );

        let mut open = server_plan(rule("serial", "16Q0A07GTM7J"));
        open.name.clear();
        open.open = vec!["name".to_owned()];
        world.request.install_plan = Some(open);
        assert_eq!(
            refusal(&mut world),
            codes::REQUEST_INVALID,
            "an open choice is not unattended"
        );

        world.request.install_plan = Some(server_plan(rule("largest", "")));
        assert_eq!(
            refusal(&mut world),
            codes::REQUEST_INVALID,
            "a staged plan names its disk, so the staging names the same one"
        );
    }

    #[test]
    fn a_plan_names_its_disk_by_a_serial_or_an_eui_and_never_another() {
        use install_profile_model::plan::DiskRule;
        let rule = |kind: &str, identity: &str| DiskRule {
            kind: kind.to_owned(),
            identity: identity.to_owned(),
        };
        let nvme = vec![
            "nvme-KIOXIA_KCD8XRUG1T92_16Q0A07GTM7J".to_owned(),
            "nvme-eui.01000000000000008ce38ee30e25a616".to_owned(),
            "wwn-0x01000000000000008ce38ee30e25a616".to_owned(),
        ];
        assert!(helper::rule_names(&rule("serial", "16Q0A07GTM7J"), &nvme));
        assert!(helper::rule_names(&rule("serial", "16q0a07gtm7j"), &nvme));
        assert!(helper::rule_names(
            &rule("wwid", "eui.01000000000000008ce38ee30e25a616"),
            &nvme
        ));
        assert!(helper::rule_names(
            &rule("wwid", "0x01000000000000008ce38ee30e25a616"),
            &nvme
        ));
        assert!(!helper::rule_names(&rule("serial", "16Q0A07GTM7X"), &nvme));
        assert!(
            !helper::rule_names(&rule("largest", ""), &nvme),
            "a rule that names no drive never matches a staging disk"
        );
        // A staging onto a named disk records that identity as the disk's
        // handle, which is no device path.
        let mut named = linux_platform().disk;
        named.id = "serial:tilo-install-target".into();
        assert_eq!(
            helper::rule_names_disk(&rule("serial", "tilo-install-target"), &named),
            Ok(true)
        );
        assert_eq!(
            helper::rule_names_disk(&rule("serial", "another-disk"), &named),
            Ok(false)
        );
        assert_eq!(
            helper::rule_names_disk(&rule("wwid", "tilo-install-target"), &named),
            Ok(false)
        );
    }

    #[test]
    fn development_layers_need_a_staged_plan() {
        let platform = linux_platform();
        let mut world = world(&platform);
        let layers = world._dir.path().join("layers");
        std::fs::create_dir(&layers).unwrap();
        std::fs::write(layers.join("components.layer"), b"layer").unwrap();
        world.request.development_layers = Some(layers.to_string_lossy().into_owned());
        let refusal = helper::validate_request(&world.request).unwrap_err();
        assert_eq!(refusal.code, codes::REQUEST_INVALID);
        assert!(
            refusal.message.contains("staged plan"),
            "development layers are carried by a staged plan only: {refusal}"
        );

        world.request.development_layers = Some("layers".into());
        world.request.install_plan = Some(server_plan(install_profile_model::plan::DiskRule {
            kind: "serial".into(),
            identity: "16Q0A07GTM7J".into(),
        }));
        let refusal = helper::validate_request(&world.request).unwrap_err();
        assert_eq!(refusal.code, codes::REQUEST_INVALID);
        assert!(
            refusal.message.contains("absolute directory path"),
            "the layers are named by an absolute directory path: {refusal}"
        );
    }

    #[test]
    fn development_layers_are_bounded_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(
            helper::development_layers(&empty)
                .unwrap_err()
                .contains("no development layers")
        );

        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("components.layer"), b"layer").unwrap();
        std::fs::write(nested.join("payload.layer"), b"layer").unwrap();
        let layers = helper::development_layers(&nested).unwrap();
        assert_eq!(
            layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            ["components.layer", "payload.layer"]
        );
        std::fs::create_dir(nested.join("subdirectory")).unwrap();
        assert!(
            helper::development_layers(&nested)
                .unwrap_err()
                .contains("only files")
        );

        #[cfg(unix)]
        {
            std::fs::remove_dir(nested.join("subdirectory")).unwrap();
            std::os::unix::fs::symlink(
                nested.join("components.layer"),
                nested.join("linked.layer"),
            )
            .unwrap();
            assert!(
                helper::development_layers(&nested)
                    .unwrap_err()
                    .contains("only files")
            );
        }

        let crowded = dir.path().join("crowded");
        std::fs::create_dir(&crowded).unwrap();
        for index in 0..=helper::MAX_DEVELOPMENT_LAYERS {
            std::fs::write(crowded.join(format!("layer-{index}")), b"layer").unwrap();
        }
        assert!(
            helper::development_layers(&crowded)
                .unwrap_err()
                .contains("more than")
        );

        let heavy = dir.path().join("heavy");
        std::fs::create_dir(&heavy).unwrap();
        let layer = std::fs::File::create(heavy.join("oversized.layer")).unwrap();
        layer
            .set_len(helper::MAX_DEVELOPMENT_LAYER_BYTES + 1)
            .unwrap();
        let refusal = helper::development_layers(&heavy).unwrap_err();
        let bound = format!(
            "more than {} of development layers",
            tilo_ui_base::format::size(helper::MAX_DEVELOPMENT_LAYER_BYTES)
        );
        assert!(refusal.contains(&bound), "{refusal}");
    }

    #[test]
    fn development_layers_reach_the_staging_root_and_a_resume_finds_them() {
        let mut platform = linux_platform();
        let world = world(&platform);
        apply(&mut platform, &world, Action::Stage).unwrap();
        let (_, root) = staged_guids(&platform, &world.request.plan);
        let disk = platform.disk.clone();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("components.layer"), b"layer one").unwrap();
        std::fs::write(dir.path().join("payload.layer"), b"layer two").unwrap();
        let layers = helper::development_layers(dir.path()).unwrap();
        helper::place_development_layers(&mut platform, &disk, &root, &layers).unwrap();

        let placed = |platform: &FakePlatform, name: &str| {
            platform
                .files
                .get(&(
                    root.to_ascii_lowercase(),
                    format!("/etc/tilo/development-layers/{name}"),
                ))
                .cloned()
        };
        assert_eq!(
            placed(&platform, "components.layer").as_deref(),
            Some(&b"layer one"[..])
        );
        assert_eq!(
            placed(&platform, "payload.layer").as_deref(),
            Some(&b"layer two"[..])
        );
        assert!(
            helper::development_layers_hold(&platform, &disk, &root, &layers).unwrap(),
            "a resumed staging finds the layers it placed"
        );

        platform
            .files
            .get_mut(&(
                root.to_ascii_lowercase(),
                "/etc/tilo/development-layers/payload.layer".to_owned(),
            ))
            .unwrap()
            .push(b' ');
        assert!(
            !helper::development_layers_hold(&platform, &disk, &root, &layers).unwrap(),
            "a changed layer is not the one the request names"
        );
        platform.files.remove(&(
            root.to_ascii_lowercase(),
            "/etc/tilo/development-layers/payload.layer".to_owned(),
        ));
        assert!(
            !helper::development_layers_hold(&platform, &disk, &root, &layers).unwrap(),
            "a missing layer is not the one the request names"
        );
    }

    #[test]
    fn full_stages_write_exact_sectors_and_follow_bundle_minimums() {
        {
            let mut p = linux_platform();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            assert_staged(&p, &w);
            let before = p.mutations;
            apply(&mut p, &w, Action::Stage).unwrap();
            assert_staged(&p, &w);
            assert!(
                p.mutations - before <= 2,
                "only the ESP record copy is refreshed"
            );
        }

        {
            let mut p = linux_4kn_platform();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            assert_staged(&p, &w);
            let (_, root) = staged_guids(&p, &w.request.plan);
            let tail = p.partition_bytes(&root, 4096);
            assert!(
                tail[w.root.len()..].iter().all(|b| *b == 0),
                "zeroed, not garbage"
            );
        }

        {
            let mut p = linux_4kn_platform();
            let before = p.clone();
            let w = world_with_esp(&p, fat::tests::staging_esp_image(false, 8192));
            let err = apply(&mut p, &w, Action::Stage).unwrap_err();
            assert_eq!(err.code, codes::SECTOR_SIZE);
            assert_eq!(p.system_disk().unwrap(), before.system_disk().unwrap());
            assert_eq!(p.vars, before.vars);
        }

        {
            let mut p = windows_platform();
            let w = world(&p);
            assert!(w.request.plan.shrink.is_some());
            assert!(w.request.plan.staging_esp.partition_guid.is_none());
            apply(&mut p, &w, Action::Stage).unwrap();
            assert_staged(&p, &w);
            let shrink = w.request.plan.shrink.as_ref().unwrap();
            assert_eq!(p.volume.as_ref().unwrap().size_bytes, shrink.to_bytes);
            let record = w.store.load(&w.request.operation_id).unwrap().unwrap();
            let (esp, _) = staged_guids(&p, &w.request.plan);
            assert_eq!(
                record.detail_str(Step::CreateEsp, "partition_guid"),
                Some(esp.as_str())
            );
        }

        {
            let p = linux_platform();
            let w = world(&p);
            let r: Requirements = w.authority.0.requirements(40 * GIB);
            assert_eq!(r.staging_esp_bytes, 2 * MIB);
            assert_eq!(r.target_bytes, 40 * GIB);
        }
    }

    #[test]
    fn a_named_disk_identity_is_revalidated_and_recorded() {
        {
            let mut platform = linux_platform();
            let mut world = world(&platform);
            world.request.plan.basis.disk.id = "serial:FAKE-DISK".into();
            world.request.plan_sha256 = world.request.plan.digest().unwrap();
            apply(&mut platform, &world, Action::Stage).unwrap();
            let staged = world
                .store
                .load(&world.request.operation_id)
                .unwrap()
                .unwrap();
            assert_eq!(staged.plan.basis.disk.id, "serial:FAKE-DISK");
            assert_eq!(staged.plan.basis.disk.guid, platform.disk.guid);
        }

        {
            // The identity is no part of the geometry the helper revalidates,
            // but a different disk is: a plan made for another disk is stale.
            let mut platform = linux_platform();
            let mut world = world(&platform);
            world.request.plan.basis.disk.guid = "99999999-8888-4777-8666-555555555555".into();
            world.request.plan_sha256 = world.request.plan.digest().unwrap();
            assert_eq!(
                apply(&mut platform, &world, Action::Stage)
                    .unwrap_err()
                    .code,
                codes::PLAN_STALE
            );
            assert_eq!(platform.mutations, 0);
        }
    }

    fn each_crash_point(total: usize, check: impl Fn(usize) + Sync) {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let next = AtomicUsize::new(1);
        let checked = AtomicUsize::new(0);
        let first_failure = Mutex::new(None);
        let workers = std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .clamp(2, 8)
            .min(total);
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| {
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        if k > total {
                            break;
                        }
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(k)));
                        checked.fetch_add(1, Ordering::Relaxed);
                        if let Err(payload) = outcome {
                            first_failure.lock().unwrap().get_or_insert(payload);
                        }
                    }
                });
            }
        });
        if let Some(payload) = first_failure.into_inner().unwrap() {
            std::panic::resume_unwind(payload);
        }
        assert_eq!(checked.into_inner(), total, "every crash point ran");
    }

    fn crash_resume_everywhere(make: fn() -> FakePlatform) {
        let mut reference = make();
        let w = world(&reference);
        apply(&mut reference, &w, Action::Stage).unwrap();
        let total = reference.mutations;
        assert!(total > 8, "{total}");
        each_crash_point(total, |k| {
            let mut p = make();
            let w = world(&p);
            p.faults.crash_after = Some(k);
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply(&mut p, &w, Action::Stage)
            }));
            assert!(crashed.is_err(), "crash {k} did not happen");
            let record = w.store.load(&w.request.operation_id).unwrap().unwrap();
            if record.status() == RecordStatus::Staged {
                assert_eq!(k, total, "crash {k} left a staged record early");
            }
            apply(&mut p, &w, Action::Stage)
                .unwrap_or_else(|r| panic!("resume after crash {k}: {r}"));
            assert_staged(&p, &w);
        });
    }

    #[test]
    fn linux_resumes_after_a_crash_at_every_mutation() {
        crash_resume_everywhere(linux_platform);
    }

    #[test]
    fn windows_resumes_after_a_crash_at_every_mutation() {
        crash_resume_everywhere(windows_platform);
    }

    #[test]
    fn an_injected_failure_is_recorded_and_retry_completes() {
        let mut p = linux_platform();
        let w = world(&p);
        p.faults.fail_at = Some(3);
        let err = apply(&mut p, &w, Action::Stage).unwrap_err();
        assert_eq!(err.code, codes::IO_FAILED);
        let record = w.store.load(&w.request.operation_id).unwrap().unwrap();
        assert!(record.steps.iter().any(|s| s.state == StepState::Failed));
        apply(&mut p, &w, Action::Stage).unwrap();
        assert_staged(&p, &w);
    }

    #[test]
    fn unavailable_firmware_keeps_geometry_but_refuses_stage_before_mutation() {
        let mut p = linux_platform();
        p.firmware_available = false;
        // Invalid boot variables must not be read as accessible firmware on
        // this host, nor mistaken for an implicit UEFI installation offer.
        p.vars.insert("BootCurrent".into(), vec![1]);
        let observed = helper::read_basis(&p).unwrap();
        assert_eq!(observed.disk, p.system_disk().unwrap());
        assert!(!observed.firmware.uefi);
        assert!(!observed.firmware.secure_boot);
        assert!(observed.firmware.boot_current.is_none());
        assert!(observed.firmware.boot_order.is_empty());
        assert!(observed.firmware.boot_next.is_none());
        let w = world(&p);
        assert!(!w.request.plan.basis.firmware.uefi);
        assert_eq!(
            apply(&mut p, &w, Action::Stage).unwrap_err().code,
            codes::REQUEST_INVALID
        );
        assert_eq!(p.mutations, 0);
        assert!(p.log.is_empty());
        assert!(w.store.load(&w.request.operation_id).unwrap().is_none());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn stale_or_tampered_inputs_are_refused_before_any_mutation() {
        {
            let mut p = linux_platform();
            let w = world(&p);
            let mut bad = w.root.clone();
            bad[0] ^= 1;
            std::fs::write(&w.root_path, &bad).unwrap();
            let err = apply(&mut p, &w, Action::Stage).unwrap_err();
            assert_eq!(err.code, codes::DIGEST_MISMATCH);
            assert_eq!(p.mutations, 0);
            assert!(w.store.load(&w.request.operation_id).unwrap().is_none());
            std::fs::write(&w.root_path, [&w.root[..], b"x"].concat()).unwrap();
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::DIGEST_MISMATCH
            );
            assert_eq!(p.mutations, 0);
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            let mut changed = p.clone();
            changed.disk.partitions.push(plan::tests::part(
                2,
                "a0000000-0000-4000-8000-000000000002",
                40 * GIB / 512,
                41 * GIB / 512,
            ));
            assert_eq!(
                apply(&mut changed, &w, Action::Stage).unwrap_err().code,
                codes::PLAN_STALE
            );
            assert_eq!(changed.mutations, 0);
            p.vars.insert("BootOrder".into(), vec![0, 0, 1, 0]);
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::PLAN_STALE
            );
            assert_eq!(p.mutations, 0);
        }

        {
            let mut p = linux_platform();
            let mut w = world(&p);
            w.request.plan.target_extent.last_lba -= 1;
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::REQUEST_INVALID
            );
            let mut w = world(&p);
            std::fs::write(&w.request.manifest_path, b"{}").unwrap();
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::MANIFEST_MISMATCH
            );
            w = world(&p);
            w.request.release.release_id = "other".into();
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::RELEASE_UNKNOWN
            );
            let mut w = world(&p);
            w.request
                .profile
                .insert("encryption_passphrase".into(), "x".into());
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::REQUEST_INVALID
            );
            assert_eq!(p.mutations, 0);
        }

        {
            let mut p = linux_platform();
            p.vars.insert("BootNext".into(), vec![0, 0]);
            let w = world(&p);
            let err = apply(&mut p, &w, Action::Stage).unwrap_err();
            assert_eq!(err.code, codes::BOOT_NEXT_BUSY);
            assert_eq!(p.mutations, 0);
        }

        {
            for tamper in 0..2 {
                let mut p = windows_platform();
                let mut w = world(&p);
                let plan = &mut w.request.plan;
                if tamper == 0 {
                    let shrink = plan.shrink.as_mut().expect("windows plan shrinks C:");
                    shrink.to_bytes -= 4 << 30;
                } else {
                    plan.target_extent.first_lba -= 2048;
                }
                w.request.plan_sha256 = w.request.plan.digest().unwrap();
                let err = apply(&mut p, &w, Action::Stage).unwrap_err();
                assert_eq!(err.code, codes::REQUEST_INVALID, "{err}");
                assert_eq!(p.mutations, 0);
            }
        }

        {
            let mut p = linux_platform();
            let mut w = world(&p);
            let mut b = fat::tests::Builder::new(false);
            b.mkdir(None, None, b"EFI        ");
            let image = b.image.clone();
            std::fs::write(&w.request.component_paths.staging_esp, &image).unwrap();
            let mut facts = w.authority.0.clone();
            facts.esp.byte_size = image.len() as u64;
            facts.esp.sha256 = sha(&image);
            w.authority = TestAuthority(facts);
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::ESP_PLACEHOLDER
            );
            assert_eq!(p.mutations, 0);
        }
    }

    #[test]
    fn pending_stages_block_and_arming_reverifies_ownership() {
        {
            let mut p = linux_platform();
            let w = world(&p);
            let mut reference = linux_platform();
            let same_plan = w_clone(&w, &reference);
            apply(&mut reference, &same_plan, Action::Stage).unwrap();
            let before_boot_next = reference
                .log
                .iter()
                .position(|l| l == "set BootNext")
                .unwrap();
            p.faults.crash_after = Some(before_boot_next);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply(&mut p, &w, Action::Stage)
            }));
            p.vars.insert("BootNext".into(), vec![0, 0]);
            let err = apply(&mut p, &w, Action::Stage).unwrap_err();
            assert_eq!(err.code, codes::BOOT_NEXT_BUSY);
            assert_eq!(p.vars["BootNext"], vec![0, 0], "never overwritten");
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            p.faults.crash_after = Some(2);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply(&mut p, &w, Action::Stage)
            }));
            let mut other = w.request.clone();
            other.operation_id = "0a0b0c0d-0000-4000-8000-000000000002".into();
            let err =
                helper::execute(&mut p, &w.store, &w.authority, &other, &mut |_| {}).unwrap_err();
            assert!(matches!(err.code, codes::PENDING_OPERATION), "{err}");
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            let before = p.clone();
            let mut other = w.request.clone();
            other.operation_id = "0a0b0c0d-0000-4000-8000-000000000003".into();
            let err =
                helper::execute(&mut p, &w.store, &w.authority, &other, &mut |_| {}).unwrap_err();
            assert_eq!(err.code, codes::PENDING_OPERATION, "{err}");
            assert_eq!(p.system_disk().unwrap(), before.system_disk().unwrap());
            apply(&mut p, &w, Action::Remove).unwrap();
            helper::execute(&mut p, &w.store, &w.authority, &other, &mut |_| {}).unwrap();
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            assert_eq!(
                apply(&mut p, &w, Action::Arm).unwrap_err().code,
                codes::NOT_STAGED
            );
            apply(&mut p, &w, Action::Stage).unwrap();
            std::fs::remove_file(&w.root_path).unwrap();
            std::fs::remove_file(&w.request.component_paths.staging_esp).unwrap();
            std::fs::remove_file(&w.request.manifest_path).unwrap();
            p.vars.remove("BootNext"); // firmware consumed it; installer cancelled
            apply(&mut p, &w, Action::Arm).unwrap();
            assert_eq!(p.vars["BootNext"], vec![0x00, 0x01]);
            p.vars.remove("BootNext");
            let (_, root) = staged_guids(&p, &w.request.plan);
            p.tamper(&root, 0, b"corrupt");
            let err = apply(&mut p, &w, Action::Arm).unwrap_err();
            assert_eq!(err.code, codes::DIGEST_MISMATCH);
            assert!(!p.vars.contains_key("BootNext"), "not armed onto bad bytes");
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            p.vars.remove("BootNext");
            let (_, root) = staged_guids(&p, &w.request.plan);
            p.tamper(&root, 0, b"grown by the live installer");
            let mut sb = vec![0_u8; 0x48];
            sb[0x20..0x30].copy_from_slice(&FIXTURE_ROOT_FSID_BYTES);
            sb[0x40..0x48].copy_from_slice(b"_BHRfS_M");
            p.tamper(&root, 0x1_0000, &sb);
            apply(&mut p, &w, Action::Arm).unwrap();
            assert_eq!(p.vars["BootNext"], vec![0x00, 0x01]);
            p.vars.remove("BootNext");
            sb[0x20] ^= 0xff;
            p.tamper(&root, 0x1_0000, &sb);
            let err = apply(&mut p, &w, Action::Arm).unwrap_err();
            assert_eq!(err.code, codes::DIGEST_MISMATCH);
            assert!(!p.vars.contains_key("BootNext"));
        }
    }

    fn w_clone(w: &World, p: &FakePlatform) -> World {
        let mut other = world(p);
        other.request.plan = w.request.plan.clone();
        other.request.plan_sha256 = w.request.plan_sha256.clone();
        other
    }

    #[test]
    fn remove_restores_owned_state_and_never_touches_changed_objects() {
        {
            let mut p = windows_platform();
            let original = p.clone();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            apply(&mut p, &w, Action::Remove).unwrap();
            assert_eq!(p.system_disk().unwrap(), original.system_disk().unwrap());
            assert_eq!(p.volume, original.volume);
            assert_eq!(p.vars, original.vars, "firmware exactly as before");
            let record = w.store.load(&w.request.operation_id).unwrap().unwrap();
            assert_eq!(record.status(), RecordStatus::Removed);
            assert!(
                super::super::record::pending_in(w.store.public_dir.as_ref().unwrap()).is_empty()
            );
            apply(&mut p, &w, Action::Remove).unwrap();
            assert_eq!(
                apply(&mut p, &w, Action::Stage).unwrap_err().code,
                codes::REQUEST_INVALID
            );
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            p.vars
                .insert("Boot0100".into(), b"someone else's entry".to_vec());
            let err = apply(&mut p, &w, Action::Remove).unwrap_err();
            assert_eq!(err.code, codes::BOOT_ENTRY_MODIFIED);
            assert_eq!(p.vars["Boot0100"], b"someone else's entry");
            let (esp, root) = staged_guids(&p, &w.request.plan);
            assert!(
                p.partition(&esp).is_ok() && p.partition(&root).is_ok(),
                "stopped before partitions"
            );
        }

        {
            let mut p = linux_platform();
            let w = world(&p);
            apply(&mut p, &w, Action::Stage).unwrap();
            let (_, root) = staged_guids(&p, &w.request.plan);
            for part in &mut p.disk.partitions {
                if part.guid == root {
                    part.type_guid = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7".into();
                }
            }
            let err = apply(&mut p, &w, Action::Remove).unwrap_err();
            assert_eq!(err.code, codes::STAGING_CHANGED);
            assert!(p.partition(&root).is_ok());
        }
    }

    #[test]
    fn remove_after_a_crash_at_every_stage_mutation_leaves_nothing() {
        let mut reference = windows_platform();
        let w = world(&reference);
        apply(&mut reference, &w, Action::Stage).unwrap();
        each_crash_point(reference.mutations, |k| {
            let mut p = windows_platform();
            let original = p.clone();
            let w = world(&p);
            p.faults.crash_after = Some(k);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply(&mut p, &w, Action::Stage)
            }));
            apply(&mut p, &w, Action::Remove)
                .unwrap_or_else(|r| panic!("remove after crash {k}: {r}"));
            assert_eq!(
                p.system_disk().unwrap(),
                original.system_disk().unwrap(),
                "crash {k}"
            );
            assert_eq!(p.vars, original.vars, "crash {k}");
            assert_eq!(p.volume, original.volume, "crash {k}");
        });
    }
}
