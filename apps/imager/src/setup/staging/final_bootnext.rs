//! Windows-direct final install: arm a single, independently verified Tilo boot.
//! This is intentionally separate from the staging firmware path. It creates
//! Boot#### and BootNext only; it never changes BootOrder or BootCurrent.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::efi::{self, HardDriveLoadOption};
use super::platform::{Disk, IoResult, Platform};
use super::{ESP_TYPE, fat, same_guid};

#[cfg(windows)]
impl FinalBootPlatform for super::windows::WindowsPlatform {
    fn read_esp_bytes(
        &self,
        disk: &Disk,
        esp_guid: &str,
        offset: u64,
        data: &mut [u8],
    ) -> IoResult<()> {
        self.read_installed_esp(disk, esp_guid, offset, data)
    }

    fn loader_sha256(&self, disk: &Disk, esp_guid: &str, path: &str) -> IoResult<String> {
        self.installed_esp_loader_sha256(disk, esp_guid, path)
    }
}

const ARG: &str = "--tilo-final-bootnext";
const PRIMARY: &str = "\\EFI\\systemd\\systemd-bootaa64.efi";
const FALLBACK: &str = "\\EFI\\BOOT\\BOOTAA64.EFI";
const MAX_LOADER: u64 = 64 << 20;
const LINUX_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const TILO_ROOT_TYPE: &str = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab";

#[derive(Clone, Debug, Deserialize)]
pub struct FinalReadback {
    pub schema: String,
    pub disk: ReviewedDisk,
    pub physical_readback_verified: bool,
    pub r#final: Vec<FinalPartition>,
    pub final_esp_partuuid: String,
    pub loader_path: String,
    pub loader_sha256: String,
    pub ci_index_digest: String,
    pub ci_child_digest: String,
    pub installed_uki_sha256: String,
    pub vm_proof_checked_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FinalPartition {
    pub role: String,
    pub partuuid: String,
    pub sha256: String,
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReviewedDisk {
    pub number: u32,
    pub unique_id: String,
    /// GPT header GUID, independently read by the physical copier.
    pub disk_guid: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArmedBoot {
    pub schema: &'static str,
    pub boot_number: String,
    pub esp_partuuid: String,
    pub loader_path: String,
    pub loader_sha256: String,
    pub boot_order_unchanged: bool,
}

/// A platform may read only the verified installed ESP and firmware. The
/// Windows adapter checks both Storage metadata and on-disk GPT each time.
pub trait FinalBootPlatform: Platform {
    fn read_esp_bytes(
        &self,
        disk: &Disk,
        esp_guid: &str,
        offset: u64,
        data: &mut [u8],
    ) -> IoResult<()>;

    fn loader_sha256(&self, disk: &Disk, esp_guid: &str, path: &str) -> IoResult<String> {
        loader_hash(self, disk, esp_guid, path)
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn checked_esp<'a>(
    disk: &'a Disk,
    receipt: &FinalReadback,
) -> IoResult<&'a super::platform::Partition> {
    if receipt.schema != "tilo.windows-direct-final-readback.v2"
        || !receipt.physical_readback_verified
        || !valid_hash(&receipt.loader_sha256)
        || !valid_hash(&receipt.installed_uki_sha256)
        || !valid_hash(&receipt.vm_proof_checked_sha256)
        || !receipt
            .ci_index_digest
            .strip_prefix("sha256:")
            .is_some_and(valid_hash)
        || !receipt
            .ci_child_digest
            .strip_prefix("sha256:")
            .is_some_and(valid_hash)
    {
        return Err("final readback is missing verified CI image evidence".into());
    }
    if receipt.loader_path != PRIMARY && receipt.loader_path != FALLBACK {
        return Err("loader path is not the reviewed ARM64 systemd-boot path".into());
    }
    if disk.id != receipt.disk.number.to_string()
        || receipt.disk.unique_id.trim().is_empty()
        || efi::guid_bytes(&receipt.disk.disk_guid).is_err()
        || !same_guid(&disk.guid, &receipt.disk.disk_guid)
        || disk.size_bytes != receipt.disk.size_bytes
    {
        return Err("Windows disk no longer matches the final readback".into());
    }
    if receipt.r#final.len() != 3 || disk.sector_bytes == 0 {
        return Err("final readback lacks the three installed partitions".into());
    }
    let expected = [
        ("final-esp", ESP_TYPE),
        ("final-boot", LINUX_TYPE),
        ("final-root", TILO_ROOT_TYPE),
    ];
    let mut esp = None;
    for (row, (role, kind)) in receipt.r#final.iter().zip(expected) {
        if row.role != role || !valid_hash(&row.sha256) || row.size_bytes == 0 {
            return Err("final readback partition entry is invalid".into());
        }
        let matching: Vec<_> = disk
            .partitions
            .iter()
            .filter(|part| same_guid(&part.guid, &row.partuuid))
            .collect();
        if matching.len() != 1 {
            return Err("final readback PARTUUID is missing or ambiguous".into());
        }
        let part = matching[0];
        let offset = part.first_lba.checked_mul(disk.sector_bytes);
        let size = part
            .last_lba
            .checked_sub(part.first_lba)
            .and_then(|sectors| sectors.checked_add(1))
            .and_then(|sectors| sectors.checked_mul(disk.sector_bytes));
        if part.number == 0
            || part.first_lba == 0
            || !same_guid(&part.type_guid, kind)
            || offset != Some(row.offset_bytes)
            || size != Some(row.size_bytes)
        {
            return Err(format!(
                "{role} geometry differs from final physical readback"
            ));
        }
        if role == "final-esp" {
            if !same_guid(&row.partuuid, &receipt.final_esp_partuuid) {
                return Err("installed ESP differs from final readback".into());
            }
            esp = Some(part);
        }
    }
    esp.ok_or_else(|| "installed ESP PARTUUID is missing".into())
}

fn loader_hash<P: FinalBootPlatform + ?Sized>(
    platform: &P,
    disk: &Disk,
    esp_guid: &str,
    path: &str,
) -> IoResult<String> {
    let esp = disk
        .partitions
        .iter()
        .find(|p| same_guid(&p.guid, esp_guid))
        .ok_or("ESP disappeared")?;
    let image_len = (esp.last_lba - esp.first_lba + 1)
        .checked_mul(disk.sector_bytes)
        .ok_or("ESP length overflow")?;
    let sector = disk.sector_bytes;
    if sector == 0 || !sector.is_power_of_two() || sector > 4096 {
        return Err("invalid disk sector size".into());
    }
    let mut read = |offset: u64, data: &mut [u8]| -> IoResult<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or("ESP offset overflow")?;
        if end > image_len {
            return Err("read exceeds installed ESP".into());
        }
        let start = offset / sector * sector;
        let aligned_end = end
            .div_ceil(sector)
            .checked_mul(sector)
            .ok_or("ESP alignment overflow")?;
        let mut aligned = vec![0; usize::try_from(aligned_end - start).map_err(|e| e.to_string())?];
        platform.read_esp_bytes(disk, esp_guid, start, &mut aligned)?;
        let within = usize::try_from(offset - start).map_err(|e| e.to_string())?;
        data.copy_from_slice(&aligned[within..within + data.len()]);
        Ok(())
    };
    hash_esp_file(&mut read, image_len, path)
}

pub(crate) fn hash_esp_file(
    read: &mut fat::ReadAt<'_>,
    image_len: u64,
    path: &str,
) -> IoResult<String> {
    let ranges = fat::file_ranges(read, image_len, &path.replace('\\', "/"))?;
    let total: u64 = ranges.iter().map(|range| range.len).sum();
    if total == 0 || total > MAX_LOADER {
        return Err("installed loader has invalid size".into());
    }
    let mut hasher = Sha256::new();
    for range in ranges {
        let mut at = range.offset;
        let end = at + range.len;
        while at < end {
            let mut chunk =
                vec![0; usize::try_from((end - at).min(1 << 20)).map_err(|e| e.to_string())?];
            read(at, &mut chunk)?;
            hasher.update(&chunk);
            at += chunk.len() as u64;
        }
    }
    Ok(efi::hex(&hasher.finalize()))
}

pub fn arm<P: FinalBootPlatform>(platform: &mut P, receipt: &FinalReadback) -> IoResult<ArmedBoot> {
    let disk = platform.system_disk()?;
    let esp = checked_esp(&disk, receipt)?;
    let expected_guid = esp.guid.clone();
    let expected_hash = receipt.loader_sha256.to_ascii_lowercase();
    if platform.loader_sha256(&disk, &expected_guid, &receipt.loader_path)? != expected_hash {
        return Err("installed ESP loader does not match final readback".into());
    }
    // A fallback is valid only when the primary path is genuinely absent.
    if receipt.loader_path == FALLBACK {
        match platform.loader_sha256(&disk, &expected_guid, PRIMARY) {
            Ok(_) => return Err("refusing fallback while primary ARM64 loader exists".into()),
            Err(error)
                if error.contains("has no placeholder")
                    || error.contains("has no directory \"systemd\"") => {}
            Err(error) => {
                return Err(format!(
                    "cannot establish that primary loader is absent: {error}"
                ));
            }
        }
    }
    let order = platform
        .get_efi_var("BootOrder")?
        .ok_or("BootOrder is absent")?;
    let parsed_order = efi::parse_u16_list(&order)?;
    if platform.get_efi_var("BootNext")?.is_some() {
        return Err("BootNext is occupied; refusing to replace it".into());
    }
    let option = efi::encode_load_option(&HardDriveLoadOption {
        description: "Tilo".into(),
        partition_number: esp.number,
        start_lba: esp.first_lba,
        size_lba: esp.last_lba - esp.first_lba + 1,
        partition_guid: expected_guid.clone(),
        file_path: receipt.loader_path.clone(),
    })?;
    let mut reused = None;
    // Reuse only byte-identical options, including their GPT device path.
    // Never change an existing option just because its description is Tilo.
    for &number in &parsed_order {
        if platform
            .get_efi_var(&efi::boot_var_name(number))?
            .as_deref()
            == Some(option.as_slice())
        {
            reused = Some(number);
            break;
        }
    }
    // Inspect a bounded owned range even across holes left by deleted entries.
    // Avoid sweeping all 65K firmware variable names on every switch.
    if reused.is_none() {
        for number in efi::FIRST_OWNED_BOOT_NUMBER..=efi::FIRST_OWNED_BOOT_NUMBER + 255 {
            if platform
                .get_efi_var(&efi::boot_var_name(number))?
                .as_deref()
                == Some(option.as_slice())
            {
                reused = Some(number);
                break;
            }
        }
    }
    let number = if let Some(number) = reused {
        number
    } else {
        efi::allocate_boot_number(&parsed_order, |number| {
            Ok(platform.get_efi_var(&efi::boot_var_name(number))?.is_some())
        })?
    };
    let name = efi::boot_var_name(number);
    // Recheck all identities immediately before the first mutation.
    let now = platform.system_disk()?;
    checked_esp(&now, receipt)?;
    if now != disk
        || platform.loader_sha256(&now, &expected_guid, &receipt.loader_path)? != expected_hash
        || platform.get_efi_var("BootOrder")?.as_deref() != Some(order.as_slice())
        || platform.get_efi_var("BootNext")?.is_some()
        || platform.get_efi_var(&name)?.as_deref() != reused.map(|_| option.as_slice())
    {
        return Err("disk, loader or firmware changed before arming".into());
    }
    if reused.is_none() {
        platform.set_efi_var(&name, &option)?;
    }
    let result = (|| -> IoResult<()> {
        if platform.get_efi_var(&name)?.as_deref() != Some(option.as_slice())
            || platform.get_efi_var("BootOrder")?.as_deref() != Some(order.as_slice())
            || platform.get_efi_var("BootNext")?.is_some()
        {
            return Err("firmware changed after Boot#### creation".into());
        }
        let now = platform.system_disk()?;
        if now != disk
            || platform.loader_sha256(&now, &expected_guid, &receipt.loader_path)? != expected_hash
        {
            return Err("installed loader changed after Boot#### creation".into());
        }
        platform.set_efi_var("BootNext", &efi::encode_u16(number))?;
        if platform.get_efi_var("BootNext")?.as_deref() != Some(efi::encode_u16(number).as_slice())
            || platform.get_efi_var("BootOrder")?.as_deref() != Some(order.as_slice())
        {
            return Err("BootNext/BootOrder readback mismatch".into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        if reused.is_none()
            && platform.get_efi_var("BootNext")?.is_none()
            && platform.get_efi_var(&name)?.as_deref() == Some(option.as_slice())
        {
            let _ = platform.delete_efi_var(&name);
        }
        return Err(error);
    }
    Ok(ArmedBoot {
        schema: "tilo.windows-direct-armed-boot.v1",
        boot_number: efi::format_boot_number(number),
        esp_partuuid: expected_guid,
        loader_path: receipt.loader_path.clone(),
        loader_sha256: expected_hash,
        boot_order_unchanged: true,
    })
}

/// Hidden helper, invoked only after the final copier has produced its
/// independently checked receipt. No firmware access occurs on other hosts.
pub fn maybe_run(args: &[String]) -> Result<Option<i32>, String> {
    if args.get(1).map(String::as_str) != Some(ARG) {
        return Ok(None);
    }
    if args.len() != 4 || args[2] != "--readback" {
        return Err("usage: imager --tilo-final-bootnext --readback FINAL-READBACK.json".into());
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        let bytes = std::fs::read(&args[3]).map_err(|e| format!("reading final readback: {e}"))?;
        if bytes.len() > 1 << 20 {
            return Err("final readback is too large".into());
        }
        let receipt: FinalReadback =
            serde_json::from_slice(&bytes).map_err(|e| format!("final readback JSON: {e}"))?;
        let mut platform = super::windows::WindowsPlatform::elevated_final_boot()?;
        let armed = arm(&mut platform, &receipt)?;
        let json = serde_json::to_vec(&armed).map_err(|e| e.to_string())?;
        std::io::stdout()
            .write_all(&json)
            .map_err(|e| e.to_string())?;
        std::io::stdout()
            .write_all(b"\n")
            .map_err(|e| e.to_string())?;
        Ok(Some(0))
    }
    #[cfg(not(windows))]
    {
        Err("the final BootNext helper requires Windows UEFI".into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::fat::tests::Builder;
    use super::super::platform::{NewPartition, Partition, SystemVolume};
    use super::*;
    use std::path::PathBuf;

    const DISK_GUID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    const ESP_GUID: &str = "11111111-2222-4333-8444-555555555555";
    const BOOT_GUID: &str = "22222222-2222-4333-8444-555555555555";
    const ROOT_GUID: &str = "33333333-2222-4333-8444-555555555555";

    /// EFI globals are files so the test covers actual readback through a
    /// filesystem, rather than a mutable HashMap view.
    struct FilePlatform {
        directory: PathBuf,
        disk: Disk,
        image: Vec<u8>,
        writes: Vec<String>,
        fail_bootnext: bool,
    }

    impl Drop for FilePlatform {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    impl FilePlatform {
        fn new(primary: bool) -> (Self, FinalReadback) {
            let directory =
                std::env::temp_dir().join(format!("tilo-bootnext-{}", super::super::random_guid()));
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("BootOrder"), [1_u8, 0]).unwrap();
            std::fs::write(directory.join("Boot0001"), b"Windows").unwrap();
            let mut fat = Builder::new(false);
            let efi = fat.mkdir(None, None, b"EFI        ");
            let dir = if primary {
                fat.mkdir(Some(efi), None, b"SYSTEMD    ")
            } else {
                fat.mkdir(Some(efi), None, b"BOOT       ")
            };
            let path = if primary { PRIMARY } else { FALLBACK };
            let short = if primary {
                b"SYSTEM~1EFI"
            } else {
                b"BOOTAA64EFI"
            };
            let long = if primary {
                Some("systemd-bootaa64.efi")
            } else {
                None
            };
            let loader = b"verified ARM64 loader";
            fat.file(Some(dir), long, short, loader, false);
            let image = fat.image;
            let first_lba = 2048;
            let esp_last = first_lba + image.len() as u64 / 512 - 1;
            let disk = Disk {
                id: "3".into(),
                guid: DISK_GUID.into(),
                size_bytes: 2 << 30,
                sector_bytes: 512,
                first_usable_lba: 34,
                last_usable_lba: (2 << 30) / 512 - 34,
                partitions: vec![
                    Partition {
                        number: 4,
                        guid: ESP_GUID.into(),
                        type_guid: ESP_TYPE.into(),
                        first_lba,
                        last_lba: esp_last,
                    },
                    Partition {
                        number: 5,
                        guid: BOOT_GUID.into(),
                        type_guid: LINUX_TYPE.into(),
                        first_lba: esp_last + 1,
                        last_lba: esp_last + 8192,
                    },
                    Partition {
                        number: 6,
                        guid: ROOT_GUID.into(),
                        type_guid: TILO_ROOT_TYPE.into(),
                        first_lba: esp_last + 8193,
                        last_lba: esp_last + 16384,
                    },
                ],
            };
            let receipt = FinalReadback {
                schema: "tilo.windows-direct-final-readback.v2".into(),
                disk: ReviewedDisk {
                    number: 3,
                    unique_id: "eui.0025380000000001".into(),
                    disk_guid: DISK_GUID.into(),
                    size_bytes: disk.size_bytes,
                },
                physical_readback_verified: true,
                r#final: disk
                    .partitions
                    .iter()
                    .zip(["final-esp", "final-boot", "final-root"])
                    .map(|(part, role)| FinalPartition {
                        role: role.into(),
                        partuuid: part.guid.clone(),
                        sha256: "e".repeat(64),
                        offset_bytes: part.first_lba * 512,
                        size_bytes: (part.last_lba - part.first_lba + 1) * 512,
                    })
                    .collect(),
                final_esp_partuuid: ESP_GUID.into(),
                loader_path: path.into(),
                loader_sha256: efi::hex(&Sha256::digest(loader)),
                ci_index_digest: format!("sha256:{}", "a".repeat(64)),
                ci_child_digest: format!("sha256:{}", "b".repeat(64)),
                installed_uki_sha256: "c".repeat(64),
                vm_proof_checked_sha256: "d".repeat(64),
            };
            (
                Self {
                    directory,
                    disk,
                    image,
                    writes: vec![],
                    fail_bootnext: false,
                },
                receipt,
            )
        }

        fn var(&self, name: &str) -> PathBuf {
            self.directory.join(name)
        }
    }

    impl Platform for FilePlatform {
        fn system_disk(&self) -> IoResult<Disk> {
            Ok(self.disk.clone())
        }
        fn system_volume(&self) -> IoResult<Option<SystemVolume>> {
            Ok(None)
        }
        fn shrink_system_volume(&mut self, _: &str, _: u64) -> IoResult<()> {
            Err("unused".into())
        }
        fn grow_system_volume(&mut self, _: &str, _: u64) -> IoResult<()> {
            Err("unused".into())
        }
        fn create_partition(&mut self, _: &Disk, _: &NewPartition) -> IoResult<String> {
            Err("unused".into())
        }
        fn delete_partition(&mut self, _: &Disk, _: &str) -> IoResult<()> {
            Err("unused".into())
        }
        fn write_partition(&mut self, _: &Disk, _: &str, _: u64, _: &[u8]) -> IoResult<()> {
            Err("unused".into())
        }
        fn read_partition(&self, _: &Disk, _: &str, _: u64, _: &mut [u8]) -> IoResult<()> {
            Err("unused".into())
        }
        fn sync_disk(&mut self, _: &Disk) -> IoResult<()> {
            Err("unused".into())
        }
        fn get_efi_var(&self, name: &str) -> IoResult<Option<Vec<u8>>> {
            match std::fs::read(self.var(name)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }
        fn set_efi_var(&mut self, name: &str, bytes: &[u8]) -> IoResult<()> {
            if name == "BootNext" && self.fail_bootnext {
                return Err("injected firmware rejection".into());
            }
            self.writes.push(name.into());
            std::fs::write(self.var(name), bytes).map_err(|e| e.to_string())
        }
        fn delete_efi_var(&mut self, name: &str) -> IoResult<()> {
            self.writes.push(format!("delete {name}"));
            std::fs::remove_file(self.var(name)).map_err(|e| e.to_string())
        }
    }

    impl FinalBootPlatform for FilePlatform {
        fn read_esp_bytes(
            &self,
            disk: &Disk,
            esp_guid: &str,
            offset: u64,
            data: &mut [u8],
        ) -> IoResult<()> {
            if disk != &self.disk
                || !same_guid(esp_guid, ESP_GUID)
                || !offset.is_multiple_of(512)
                || !data.len().is_multiple_of(512)
            {
                return Err("invalid ESP read".into());
            }
            let start = usize::try_from(offset).map_err(|e| e.to_string())?;
            data.copy_from_slice(
                self.image
                    .get(start..start + data.len())
                    .ok_or("ESP read overflow")?,
            );
            Ok(())
        }
    }

    #[test]
    fn final_bootnext_reuses_exact_options_across_holes_and_preserves_foreign_entries() {
        let (mut platform, receipt) = FilePlatform::new(true);
        arm(&mut platform, &receipt).unwrap();
        let option = platform.get_efi_var("Boot0100").unwrap().unwrap();
        std::fs::remove_file(platform.var("BootNext")).unwrap();
        platform.writes.clear();
        assert_eq!(arm(&mut platform, &receipt).unwrap().boot_number, "0100");
        assert_eq!(platform.writes, ["BootNext"]);
        std::fs::remove_file(platform.var("BootNext")).unwrap();
        std::fs::remove_file(platform.var("Boot0100")).unwrap();
        std::fs::write(platform.var("Boot0102"), &option).unwrap();
        std::fs::write(platform.var("Boot0101"), b"foreign option").unwrap();
        platform.writes.clear();
        assert_eq!(arm(&mut platform, &receipt).unwrap().boot_number, "0102");
        assert_eq!(platform.writes, ["BootNext"]);
        assert_eq!(
            platform.get_efi_var("Boot0101").unwrap().unwrap(),
            b"foreign option"
        );
        assert_eq!(platform.get_efi_var("Boot0102").unwrap().unwrap(), option);
        assert_eq!(platform.get_efi_var("BootOrder").unwrap().unwrap(), [1, 0]);
        platform.writes.clear();
        assert!(
            arm(&mut platform, &receipt)
                .unwrap_err()
                .contains("occupied")
        );
        assert!(platform.writes.is_empty());
    }

    #[test]
    fn final_bootnext_does_not_reuse_a_different_tilo_device_path() {
        let (mut platform, receipt) = FilePlatform::new(true);
        arm(&mut platform, &receipt).unwrap();
        std::fs::remove_file(platform.var("BootNext")).unwrap();
        let mut decoded =
            efi::decode_load_option(&platform.get_efi_var("Boot0100").unwrap().unwrap()).unwrap();
        decoded.partition_guid = "99999999-bbbb-4ccc-8ddd-eeeeeeeeeeee".into();
        let option = efi::encode_load_option(&decoded).unwrap();
        std::fs::write(platform.var("Boot0100"), &option).unwrap();
        platform.writes.clear();
        assert_eq!(arm(&mut platform, &receipt).unwrap().boot_number, "0101");
        assert_eq!(platform.writes, ["Boot0101", "BootNext"]);
        assert_eq!(platform.get_efi_var("Boot0100").unwrap().unwrap(), option);
    }

    #[test]
    fn final_bootnext_failed_rearm_never_deletes_the_reused_option() {
        let (mut platform, receipt) = FilePlatform::new(true);
        arm(&mut platform, &receipt).unwrap();
        let option = platform.get_efi_var("Boot0100").unwrap().unwrap();
        std::fs::remove_file(platform.var("BootNext")).unwrap();
        platform.writes.clear();
        platform.fail_bootnext = true;
        assert!(
            arm(&mut platform, &receipt)
                .unwrap_err()
                .contains("injected")
        );
        assert!(platform.writes.is_empty());
        assert_eq!(platform.get_efi_var("Boot0100").unwrap().unwrap(), option);
        assert!(platform.get_efi_var("BootNext").unwrap().is_none());
        assert_eq!(platform.get_efi_var("BootOrder").unwrap().unwrap(), [1, 0]);
    }

    /// Arming, refusals and fallback.
    #[test]
    fn final_bootnext_arms_only_verified_new_options() {
        // Phase `arms_only_new_option_and_bootnext_and_preserves_boot_order`.
        {
            let (mut platform, receipt) = FilePlatform::new(true);
            let armed = arm(&mut platform, &receipt).unwrap();
            assert_eq!(armed.boot_number, "0100");
            assert_eq!(platform.writes, ["Boot0100", "BootNext"]);
            assert_eq!(platform.get_efi_var("BootOrder").unwrap().unwrap(), [1, 0]);
            assert_eq!(platform.get_efi_var("BootNext").unwrap().unwrap(), [0, 1]);
            let option = platform.get_efi_var("Boot0100").unwrap().unwrap();
            let decoded = efi::decode_load_option(&option).unwrap();
            assert_eq!(decoded.partition_guid, ESP_GUID);
            assert_eq!(decoded.file_path, PRIMARY);
            assert_eq!(decoded.partition_number, 4);
        }

        // Hardware UniqueId is not the GPT GUID. Missing, invalid or mismatched
        // GPT identity must refuse firmware writes, even with the hardware ID.
        for guid in [
            "",
            "eui.0025380000000001",
            "99999999-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        ] {
            let (mut platform, mut receipt) = FilePlatform::new(true);
            receipt.disk.disk_guid = guid.into();
            assert!(arm(&mut platform, &receipt).unwrap_err().contains("disk"));
            assert!(platform.writes.is_empty());
        }
        {
            let (mut platform, mut receipt) = FilePlatform::new(true);
            receipt.schema = "tilo.windows-direct-final-readback.v1".into();
            assert!(arm(&mut platform, &receipt).is_err());
            assert!(platform.writes.is_empty());
        }

        // Phase `refuses_occupied_bootnext_and_identity_or_loader_mismatch`.
        {
            let (mut platform, receipt) = FilePlatform::new(true);
            std::fs::write(platform.var("BootNext"), [1, 0]).unwrap();
            assert!(
                arm(&mut platform, &receipt)
                    .unwrap_err()
                    .contains("occupied")
            );
            assert!(platform.writes.is_empty());
            std::fs::remove_file(platform.var("BootNext")).unwrap();
            let loader_offset = platform
                .image
                .windows(b"verified ARM64 loader".len())
                .position(|bytes| bytes == b"verified ARM64 loader")
                .unwrap();
            platform.image[loader_offset] ^= 1;
            assert!(arm(&mut platform, &receipt).is_err());
            assert!(platform.writes.is_empty());
            platform.disk.guid = "99999999-bbbb-4ccc-8ddd-eeeeeeeeeeee".into();
            assert!(arm(&mut platform, &receipt).unwrap_err().contains("disk"));
            assert!(platform.writes.is_empty());
        }

        // Phase `refuses_changed_boot_or_root_geometry_after_physical_readback`.
        {
            let (mut platform, mut receipt) = FilePlatform::new(true);
            platform.disk.partitions[1].last_lba += 1;
            assert!(
                arm(&mut platform, &receipt)
                    .unwrap_err()
                    .contains("final-boot geometry")
            );
            assert!(platform.writes.is_empty());
            platform.disk.partitions[1].last_lba -= 1;
            platform.disk.partitions[2].type_guid = LINUX_TYPE.into();
            assert!(
                arm(&mut platform, &receipt)
                    .unwrap_err()
                    .contains("final-root geometry")
            );
            assert!(platform.writes.is_empty());
            platform.disk.partitions[2].type_guid = TILO_ROOT_TYPE.into();
            receipt.r#final[2].offset_bytes += 512;
            assert!(
                arm(&mut platform, &receipt)
                    .unwrap_err()
                    .contains("final-root geometry")
            );
            assert!(platform.writes.is_empty());
        }

        // Phase `fallback_requires_primary_absence`.
        {
            let (mut platform, receipt) = FilePlatform::new(false);
            assert_eq!(arm(&mut platform, &receipt).unwrap().loader_path, FALLBACK);
        }
    }
}
