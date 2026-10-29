//! UEFI firmware for a QEMU guest, and the variable store it boots with.
//!
//! Fedora ships two x86 OVMF builds and only one of them can measure:
//!
//! * `OVMF_CODE_4M.secboot.qcow2`, 4 MB, TPM support compiled in. Its TCG2
//!   protocol reports an active SHA-256 PCR bank, so the firmware starts the
//!   TPM, extends PCRs 0-7 itself, writes an event log, and hands systemd-stub
//!   a TCG2 it can extend PCR 11 through.
//! * `OVMF_CODE.secboot.fd`, 2 MB, no TPM support. TCG2 reports zero active
//!   PCR banks, so nothing is ever extended: every PCR reads zero, no event log
//!   appears, the TPM is never started (Linux logs "A TPM error (256) occurred
//!   attempting the self test") and `ConditionSecurity=measured-uki` is unmet.
//!
//! That second list is exactly what a UKI which failed to measure looks like,
//! so the 4 MB build is a requirement rather than a preference: under the 2 MB
//! one os/tests/rollback-boot-check.sh reports "nothing measured into PCR 11"
//! for a kernel that measured perfectly. When a TPM is attached and the 4 MB
//! build is absent the boot is refused, with how to get it, instead of
//! producing a result that cannot mean anything.
//!
//! The secboot flavour is chosen for its measurement support, not to enforce
//! anything: Secure Boot enforcement is a property of the variable store
//! ([`Keys`]), not a QEMU flag. SMM and a write-protected pflash go with the
//! secboot build unconditionally, because that firmware expects them and a
//! variable store the guest can rewrite makes enforcement theatre.

use std::path::{Path, PathBuf};

use super::{Arch, QemuError};

/// The directories searched for x86 OVMF when none are given.
pub const OVMF_DIRS: &[&str] = &["/usr/share/edk2/ovmf", "/usr/share/OVMF"];
/// The directories searched for ARM AAVMF when none are given.
pub const AAVMF_DIRS: &[&str] = &[
    "/usr/share/edk2/aarch64",
    "/usr/share/AAVMF",
    "/usr/share/qemu-efi-aarch64",
];

const DNF_REMEDY: &str = "install edk2-ovmf (Fedora: sudo dnf install edk2-ovmf; in a distrobox: distrobox enter <box> -- sudo dnf install edk2-ovmf), or apt install ovmf (Debian/Ubuntu)";

/// Which keys the variable store starts with. This decides whether an
/// owner-signed image is bootable at all, so it is a deliberate choice rather
/// than whichever template happens to sit next to the firmware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keys {
    /// `OVMF_VARS_4M.qcow2`: no keys, Secure Boot in setup mode. Measures into
    /// PCR 7 like any other store and accepts an owner-signed UKI. Re-seeded
    /// from the template on every boot unless the store is kept.
    None,
    /// The same pristine template, kept across boots so that an enrolment in
    /// the guest (`/usr/libexec/tilo-secureboot enroll`) survives a restart.
    Persist,
    /// Microsoft's KEK and db. Not "the secure one": a Tilo UKI is signed with
    /// the owner's key, which that db does not cover, so nothing boots and the
    /// firmware says nothing about why. Selecting it is an explicit request to
    /// test vendor-key enforcement.
    Microsoft,
    /// A prepared store, used as-is and kept across boots.
    Store(PathBuf),
}

impl Keys {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "none" | "" => Self::None,
            "persist" => Self::Persist,
            "microsoft" => Self::Microsoft,
            path => Self::Store(PathBuf::from(path)),
        }
    }

    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::None => "none".into(),
            Self::Persist => "persist".into(),
            Self::Microsoft => "microsoft".into(),
            Self::Store(path) => path.display().to_string(),
        }
    }

    /// Kept across boots rather than re-seeded from the template.
    fn persistent(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// What the firmware is asked to support.
#[derive(Debug, Clone)]
pub struct Request {
    pub arch: Arch,
    pub tpm: bool,
    /// Keep the pristine store across boots so an enrolment sticks
    /// (`Keys::None` becomes `Keys::Persist`).
    pub secboot: bool,
    pub keys: Keys,
    /// Offer the guest suspend-to-RAM, which needs a build without SMM.
    pub s3: bool,
    pub ovmf_dirs: Vec<PathBuf>,
    pub aavmf_dirs: Vec<PathBuf>,
}

/// The firmware a request resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    pub code: PathBuf,
    /// The variable-store template (or the prepared store).
    pub vars: PathBuf,
    /// `qcow2` or `raw`: the pflash format, part of the answer, because the
    /// wrong `format=` makes QEMU read firmware as garbage.
    pub format: &'static str,
    pub keys: Keys,
    /// A build verified to measure into an attached TPM.
    pub measures: bool,
    /// Built with SMM: the machine needs `smm=on` and a secure pflash.
    pub smm: bool,
    /// What the selection means, for the person reading the log.
    pub notes: Vec<String>,
}

/// Resolve the firmware for `request`; `exists` says whether a file is there.
///
/// # Errors
/// No firmware, a missing variable store, or a combination that could not
/// mean anything (a TPM on a firmware that cannot measure).
#[expect(
    clippy::too_many_lines,
    reason = "one decision table, kept in one place"
)]
pub fn resolve(request: &Request, exists: &dyn Fn(&Path) -> bool) -> Result<Firmware, QemuError> {
    let mut keys = request.keys.clone();
    let mut notes = Vec::new();
    if request.secboot && keys == Keys::None {
        keys = Keys::Persist;
        notes.push(
            "--secboot: the variable store persists across boots so an enrolment sticks".into(),
        );
    }
    let first = |candidates: &[PathBuf]| candidates.iter().find(|path| exists(path)).cloned();
    let dirs = |dirs: &[PathBuf], defaults: &[&str]| -> Vec<PathBuf> {
        if dirs.is_empty() {
            defaults.iter().map(PathBuf::from).collect()
        } else {
            dirs.to_vec()
        }
    };

    if request.arch == Arch::Aarch64 {
        // AAVMF/QEMU_EFI is distributed under different names by Fedora and
        // Debian-family hosts; both put a blank variable-store template next
        // to the read-only code image. Measured boot is never implied from an
        // arbitrary AAVMF build merely because it exposes a TPM device: the
        // TPM is there for what does not bind to PCRs, the TPM-and-secret
        // wrapper of sign-in (docs/security/sign-in.md §4.1), and anything
        // PCR-bound tested on it proves nothing.
        if request.tpm {
            notes.push(
                "tpm: swtpm on tpm-tis-device; AAVMF's measurements are unverified, so only what is not PCR-bound is tested".into(),
            );
        }
        if keys != Keys::None {
            return Err(QemuError::new(
                "aarch64 VM Secure Boot key stores are not verified yet",
                "boot the ARM guest with the default keys (none)",
            ));
        }
        let candidates: Vec<PathBuf> = dirs(&request.aavmf_dirs, AAVMF_DIRS)
            .iter()
            .flat_map(|dir| [dir.join("QEMU_EFI-pflash.raw"), dir.join("AAVMF_CODE.fd")])
            .collect();
        let code = first(&candidates).ok_or_else(|| {
            QemuError::new(
                "no AAVMF firmware found",
                "install edk2-aarch64 (Fedora) or qemu-efi-aarch64 (Debian/Ubuntu), or name its directory with --aavmf-dir",
            )
        })?;
        let template = match code.file_name().and_then(|n| n.to_str()) {
            Some("QEMU_EFI-pflash.raw") => "vars-template-pflash.raw",
            _ => "AAVMF_VARS.fd",
        };
        let vars = code.with_file_name(template);
        if !exists(&vars) {
            return Err(QemuError::new(
                format!("AAVMF variable-store template missing: {}", vars.display()),
                "reinstall the AAVMF package; its variable-store template sits next to the code image",
            ));
        }
        notes.push(format!(
            "firmware: {} [raw] keys=none measures=unverified smm=0",
            code.display()
        ));
        return Ok(Firmware {
            code,
            vars,
            format: "raw",
            keys,
            measures: false,
            smm: false,
            notes,
        });
    }

    let ovmf = dirs(&request.ovmf_dirs, OVMF_DIRS);
    if request.s3 && !matches!(keys, Keys::None | Keys::Persist) {
        return Err(QemuError::new(
            format!(
                "suspend-to-RAM boots the firmware without Secure Boot; it cannot hold the '{}' keys",
                keys.name()
            ),
            "drop --s3, or use --keys none/persist",
        ));
    }
    // S3 needs the build without SMM: the SMM-protected store and S3 resume
    // do not coexist. Same 4 MB measuring build otherwise.
    let measuring = if request.s3 {
        "OVMF_CODE_4M.qcow2"
    } else {
        "OVMF_CODE_4M.secboot.qcow2"
    };
    let candidates: Vec<PathBuf> = ovmf.iter().map(|dir| dir.join(measuring)).collect();
    let (code, format, measures, smm, vars) = if let Some(code) = first(&candidates) {
        let vars = match &keys {
            Keys::None | Keys::Persist => code.with_file_name("OVMF_VARS_4M.qcow2"),
            Keys::Microsoft => code.with_file_name("OVMF_VARS_4M.secboot.qcow2"),
            Keys::Store(path) => path.clone(),
        };
        (code, "qcow2", true, !request.s3, vars)
    } else {
        // Debian and Ubuntu ship the 4 MB build as raw .fd (OVMF_CODE_4M*.fd
        // with OVMF_VARS_4M*.fd) and no 2 MB one; Fedora's 2 MB build is
        // OVMF_CODE*.fd. Their .secboot builds carry SMM, which S3 resume
        // cannot coexist with.
        // Name-major order (every directory's preferred builds before any
        // directory's next one), except that S3 tries both per directory.
        let candidates: Vec<PathBuf> = if request.s3 {
            ovmf.iter()
                .flat_map(|dir| [dir.join("OVMF_CODE_4M.fd"), dir.join("OVMF_CODE.fd")])
                .collect()
        } else {
            let mut list: Vec<PathBuf> = ovmf
                .iter()
                .flat_map(|dir| {
                    [
                        dir.join("OVMF_CODE_4M.secboot.fd"),
                        dir.join("OVMF_CODE_4M.fd"),
                    ]
                })
                .collect();
            list.extend(ovmf.iter().map(|dir| dir.join("OVMF_CODE.secboot.fd")));
            list.extend(ovmf.iter().map(|dir| dir.join("OVMF_CODE.fd")));
            list
        };
        let Some(code) = first(&candidates) else {
            let looked = ovmf
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(" ");
            return Err(if request.s3 {
                QemuError::new(
                    format!("suspend-to-RAM needs an OVMF build without SMM (looked in {looked})"),
                    "install OVMF_CODE_4M.qcow2 (Fedora edk2-ovmf) or OVMF_CODE_4M.fd (Debian ovmf)",
                )
            } else {
                QemuError::new(format!("no OVMF firmware (looked in {looked})"), DNF_REMEDY)
            });
        };
        let name = code
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let four_mb = name.contains("_4M");
        if request.tpm && four_mb {
            return Err(QemuError::new(
                format!(
                    "the OVMF build here ({}) is the Debian-family 4 MB raw image, whose measured boot is not verified; refusing to attach a TPM whose PCRs would not be evidence of anything",
                    code.display()
                ),
                "use a host with Fedora's edk2-ovmf, which ships /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 + OVMF_VARS_4M.qcow2",
            ));
        }
        if request.tpm {
            return Err(QemuError::new(
                format!(
                    "the only OVMF build here is the 2 MB one ({}), which has no TPM support: its TCG2 protocol reports zero active PCR banks, so every PCR would read zero, the same evidence a UKI that failed to measure leaves behind; refusing to attach a TPM to a firmware that cannot measure into it",
                    code.display()
                ),
                "install a current edk2-ovmf; it ships /usr/share/edk2/ovmf/OVMF_CODE_4M.secboot.qcow2 + OVMF_VARS_4M.qcow2",
            ));
        }
        let smm = name.ends_with(".secboot.fd");
        let vars = match (&keys, four_mb) {
            (Keys::None | Keys::Persist, true) => code.with_file_name("OVMF_VARS_4M.fd"),
            (Keys::Microsoft, true) => code.with_file_name("OVMF_VARS_4M.ms.fd"),
            (Keys::None | Keys::Persist, false) => code.with_file_name("OVMF_VARS.fd"),
            (Keys::Microsoft, false) => code.with_file_name("OVMF_VARS.secboot.fd"),
            (Keys::Store(path), _) => path.clone(),
        };
        if four_mb {
            notes.push(format!(
                "the Debian-family OVMF build ({}) is not a verified measuring firmware here: no PCR, event-log or measured-boot result from this VM is evidence",
                code.display()
            ));
        } else {
            notes.push(format!(
                "the 2 MB OVMF build ({}) cannot measure: no PCR, event-log or measured-boot result from this VM is evidence of anything",
                code.display()
            ));
        }
        (code, "raw", false, smm, vars)
    };

    if !exists(&vars) {
        return Err(match keys {
            Keys::Store(_) => QemuError::new(
                format!("the variable store {} does not exist", vars.display()),
                "enrolling an owner key is a deliberate step: boot with --secboot (pristine store, setup mode, kept across boots), run `/usr/libexec/tilo-secureboot enroll` in the guest, and reboot the guest; the store then carries the owner PK/KEK/db",
            ),
            _ => QemuError::new(
                format!("OVMF variable-store template missing: {}", vars.display()),
                DNF_REMEDY,
            ),
        });
    }
    if keys == Keys::Microsoft {
        notes.push(format!(
            "variable store: Microsoft's keys ({}). The firmware refuses an image signed with any other key before it prints anything, so an owner-signed UKI produces a guest that never appears. Use --keys none (setup mode, still measures) unless you are testing vendor-key enforcement",
            vars.display()
        ));
    }
    notes.push(format!(
        "firmware: {} [{format}] keys={} measures={} smm={}",
        code.display(),
        keys.name(),
        u8::from(measures),
        u8::from(smm)
    ));
    Ok(Firmware {
        code,
        vars,
        format,
        keys,
        measures,
        smm,
        notes,
    })
}

impl Firmware {
    /// The working variable store under `run`. Without `keep`, the default
    /// keys (`none`) are re-seeded from the template on every boot so every
    /// boot starts from the same firmware state; every other selection is
    /// kept, because an enrolment a restart silently threw away is
    /// indistinguishable from one that never happened.
    #[must_use]
    pub fn varstore_path(&self, run: &Path) -> PathBuf {
        let tag: String = self
            .keys
            .name()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        run.join(format!("vars-{}-{tag}.img", self.format))
    }

    /// Seed the working variable store (see [`Self::varstore_path`]).
    ///
    /// # Errors
    /// The template could not be copied.
    pub fn seed_varstore(&self, run: &Path, keep: bool) -> Result<PathBuf, QemuError> {
        let out = self.varstore_path(run);
        std::fs::create_dir_all(run).map_err(QemuError::io(run))?;
        if !(keep || self.keys.persistent()) || !out.exists() {
            std::fs::copy(&self.vars, &out).map_err(QemuError::io(&out))?;
            // A template from a read-only package keeps its mode in the copy.
            let mut permissions = std::fs::metadata(&out)
                .map_err(QemuError::io(&out))?
                .permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "the copy is this VM's own store, which the firmware writes"
            )]
            permissions.set_readonly(false);
            std::fs::set_permissions(&out, permissions).map_err(QemuError::io(&out))?;
        }
        Ok(out)
    }

    /// The two pflash drives: read-only code, then the writable store.
    #[must_use]
    pub fn pflash_args(&self, varstore: &Path) -> Vec<String> {
        vec![
            "-drive".into(),
            format!(
                "if=pflash,format={},readonly=on,file={}",
                self.format,
                super::escape(&self.code.display().to_string())
            ),
            "-drive".into(),
            format!(
                "if=pflash,format={},file={}",
                self.format,
                super::escape(&varstore.display().to_string())
            ),
        ]
    }

    /// The table `tilo-vm qemu firmware` prints.
    #[must_use]
    pub fn table(&self, tpm: bool) -> String {
        format!(
            "code       {}\nvars       {}\nformat     {}\nkeys       {}\nmeasures   {}\nsmm        {}\ntpm        {}",
            self.code.display(),
            self.vars.display(),
            self.format,
            self.keys.name(),
            u8::from(self.measures),
            u8::from(self.smm),
            u8::from(tpm)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn request(arch: Arch, dir: &str) -> Request {
        Request {
            arch,
            tpm: false,
            secboot: false,
            keys: Keys::None,
            s3: false,
            ovmf_dirs: vec![PathBuf::from(dir)],
            aavmf_dirs: vec![PathBuf::from(dir)],
        }
    }

    fn files(dir: &str, names: &[&str]) -> BTreeSet<PathBuf> {
        names.iter().map(|n| Path::new(dir).join(n)).collect()
    }

    fn pick(
        request: &Request,
        present: &BTreeSet<PathBuf>,
    ) -> Result<(String, String, &'static str, bool, bool), QemuError> {
        resolve(request, &|p| present.contains(p)).map(|f| {
            let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
            (name(&f.code), name(&f.vars), f.format, f.measures, f.smm)
        })
    }

    #[test]
    fn debian_raw_build_is_found_with_its_store_and_smm() {
        let present = files(
            "/d",
            &[
                "OVMF_CODE_4M.fd",
                "OVMF_CODE_4M.secboot.fd",
                "OVMF_VARS_4M.fd",
                "OVMF_VARS_4M.ms.fd",
            ],
        );
        let r = request(Arch::X86_64, "/d");
        assert_eq!(
            pick(&r, &present).unwrap(),
            (
                "OVMF_CODE_4M.secboot.fd".into(),
                "OVMF_VARS_4M.fd".into(),
                "raw",
                false,
                true
            )
        );
        // A TPM is refused on the unverified Debian-family build.
        assert!(
            pick(
                &Request {
                    tpm: true,
                    ..r.clone()
                },
                &present
            )
            .is_err()
        );
        // Suspend-to-RAM takes the build without SMM.
        assert_eq!(
            pick(&Request { s3: true, ..r }, &present).unwrap(),
            (
                "OVMF_CODE_4M.fd".into(),
                "OVMF_VARS_4M.fd".into(),
                "raw",
                false,
                false
            )
        );
    }

    #[test]
    fn fedora_measuring_build_wins_and_s3_drops_smm() {
        let mut present = files(
            "/f",
            &[
                "OVMF_CODE_4M.secboot.qcow2",
                "OVMF_VARS_4M.qcow2",
                "OVMF_CODE_4M.fd",
            ],
        );
        let r = request(Arch::X86_64, "/f");
        assert_eq!(
            pick(&r, &present).unwrap(),
            (
                "OVMF_CODE_4M.secboot.qcow2".into(),
                "OVMF_VARS_4M.qcow2".into(),
                "qcow2",
                true,
                true
            )
        );
        present.insert(PathBuf::from("/f/OVMF_CODE_4M.qcow2"));
        assert_eq!(
            pick(
                &Request {
                    s3: true,
                    ..r.clone()
                },
                &present
            )
            .unwrap(),
            (
                "OVMF_CODE_4M.qcow2".into(),
                "OVMF_VARS_4M.qcow2".into(),
                "qcow2",
                true,
                false
            )
        );
        // The TPM is accepted on the measuring build.
        assert!(
            pick(
                &Request {
                    tpm: true,
                    ..r.clone()
                },
                &present
            )
            .is_ok()
        );
        // Microsoft keys select the vendor store, which must exist.
        assert!(
            pick(
                &Request {
                    keys: Keys::Microsoft,
                    ..r
                },
                &present
            )
            .is_err()
        );
    }

    #[test]
    fn s3_refuses_smm_only_firmware_and_nothing_found_stops() {
        let present = files("/s", &["OVMF_CODE_4M.secboot.fd", "OVMF_VARS_4M.fd"]);
        assert!(
            pick(
                &Request {
                    s3: true,
                    ..request(Arch::X86_64, "/s")
                },
                &present
            )
            .is_err()
        );
        assert!(pick(&request(Arch::X86_64, "/empty"), &BTreeSet::new()).is_err());
    }

    #[test]
    fn two_megabyte_build_refuses_a_tpm() {
        let present = files("/o", &["OVMF_CODE.fd", "OVMF_VARS.fd"]);
        let r = request(Arch::X86_64, "/o");
        assert_eq!(
            pick(&r, &present).unwrap(),
            (
                "OVMF_CODE.fd".into(),
                "OVMF_VARS.fd".into(),
                "raw",
                false,
                false
            )
        );
        let error = pick(&Request { tpm: true, ..r }, &present).unwrap_err();
        assert!(error.detail.contains("2 MB"), "{error:?}");
    }

    #[test]
    fn arm_firmware_matches_both_package_layouts_and_takes_a_tpm_unmeasured() {
        let fedora = files("/a", &["QEMU_EFI-pflash.raw", "vars-template-pflash.raw"]);
        let debian = files("/a", &["AAVMF_CODE.fd", "AAVMF_VARS.fd"]);
        let r = request(Arch::Aarch64, "/a");
        assert_eq!(
            pick(&r, &fedora).unwrap(),
            (
                "QEMU_EFI-pflash.raw".into(),
                "vars-template-pflash.raw".into(),
                "raw",
                false,
                false
            )
        );
        assert_eq!(
            pick(&r, &debian).unwrap(),
            (
                "AAVMF_CODE.fd".into(),
                "AAVMF_VARS.fd".into(),
                "raw",
                false,
                false
            )
        );
        let with_tpm = resolve(&Request { tpm: true, ..r }, &|p| fedora.contains(p)).unwrap();
        assert!(
            !with_tpm.measures,
            "an AAVMF build is never taken to measure"
        );
        assert!(
            with_tpm
                .notes
                .iter()
                .any(|note| note.contains("not PCR-bound")),
            "{:?}",
            with_tpm.notes
        );
    }

    #[test]
    fn secboot_keeps_the_pristine_store_and_a_prepared_store_must_exist() {
        let present = files("/f", &["OVMF_CODE_4M.secboot.qcow2", "OVMF_VARS_4M.qcow2"]);
        let r = request(Arch::X86_64, "/f");
        let firmware = resolve(
            &Request {
                secboot: true,
                ..r.clone()
            },
            &|p| present.contains(p),
        )
        .unwrap();
        assert_eq!(firmware.keys, Keys::Persist);
        assert_eq!(
            firmware.varstore_path(Path::new("/run")),
            Path::new("/run/vars-qcow2-persist.img")
        );
        let error = resolve(
            &Request {
                keys: Keys::Store("/nowhere/vars.qcow2".into()),
                ..r
            },
            &|p| present.contains(p),
        )
        .unwrap_err();
        assert!(error.remedy.contains("tilo-secureboot enroll"), "{error:?}");
    }

    #[test]
    fn default_store_is_reseeded_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let template = dir.path().join("OVMF_VARS_4M.qcow2");
        std::fs::write(&template, b"template").unwrap();
        let firmware = Firmware {
            code: dir.path().join("OVMF_CODE_4M.secboot.qcow2"),
            vars: template,
            format: "qcow2",
            keys: Keys::None,
            measures: true,
            smm: true,
            notes: Vec::new(),
        };
        let run = dir.path().join("run");
        let store = firmware.seed_varstore(&run, false).unwrap();
        std::fs::write(&store, b"enrolled").unwrap();
        firmware.seed_varstore(&run, true).unwrap();
        assert_eq!(std::fs::read(&store).unwrap(), b"enrolled");
        firmware.seed_varstore(&run, false).unwrap();
        assert_eq!(std::fs::read(&store).unwrap(), b"template");
    }
}
