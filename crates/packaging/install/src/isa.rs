//! Detect the local CPU's highest supported architecture-specific
//! feature level.
//!
//! Two-arch today (`x86_64`, aarch64); adding a new arch is a matching
//! extension to `pkg::manifest::Arch`, a new detector function here,
//! plus a `pick_variant` case.
//!
//! # How it works
//!
//! Reads CPU feature flags out of `/proc/cpuinfo` — the kernel already
//! parses CPUID (x86) / MIDR + `Features:` (aarch64) and exposes them
//! there. Prefer this over inline asm because it's testable (feed a
//! synthetic flags line and assert the mapping) and works from any
//! userland.

use pkg::manifest::{AArch64Level, Arch, IsaLevel, IsaRequirements, X86_64Level};
use std::fs;

/// Detect the local host's arch + best-fit level for that arch.
pub fn detect() -> IsaLevel {
    detect_from_cpuinfo(
        &fs::read_to_string("/proc/cpuinfo").unwrap_or_default(),
        host_arch(),
    )
}

/// Report the host arch. Cargo compiles a per-arch binary so
/// `cfg!(target_arch = "…")` is authoritative for this process's
/// arch — the CPU it's running on can't differ.
pub fn host_arch() -> Arch {
    #[cfg(target_arch = "x86_64")]
    {
        Arch::X86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        Arch::AArch64
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        panic!("qOS binary compiled for unsupported target arch");
    }
}

/// Public for tests + for the CLI's `qos-install isa-report`.
pub fn detect_from_cpuinfo(cpuinfo: &str, arch: Arch) -> IsaLevel {
    let flags = extract_flags(cpuinfo, arch);
    match arch {
        Arch::X86_64 => IsaLevel::X86_64(detect_x86_64(flags)),
        Arch::AArch64 => IsaLevel::AArch64(detect_aarch64(flags)),
    }
}

/// The kernel names the feature line `flags:` on x86 and `Features:`
/// on aarch64 — same shape, different label.
fn extract_flags(cpuinfo: &str, arch: Arch) -> &str {
    let key = match arch {
        Arch::X86_64 => "flags",
        Arch::AArch64 => "Features",
    };
    for line in cpuinfo.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        }
    }
    ""
}

fn has(flags: &str, needle: &str) -> bool {
    flags.split_whitespace().any(|f| f == needle)
}

fn detect_x86_64(flags: &str) -> X86_64Level {
    let v4 = ["avx512f", "avx512bw", "avx512cd", "avx512dq", "avx512vl"]
        .iter()
        .all(|f| has(flags, f));
    let v3_extras = [
        "avx", "avx2", "bmi1", "bmi2", "f16c", "fma", "abm", "movbe", "xsave",
    ]
    .iter()
    .all(|f| has(flags, f));
    let v2_extras = ["sse3", "ssse3", "sse4_1", "sse4_2", "popcnt", "cx16"]
        .iter()
        .all(|f| has(flags, f));

    match (v4, v3_extras && v2_extras, v2_extras) {
        (true, true, true) => X86_64Level::V4,
        (_, true, true) => X86_64Level::V3,
        (_, _, true) => X86_64Level::V2,
        _ => X86_64Level::Baseline,
    }
}

fn detect_aarch64(flags: &str) -> AArch64Level {
    // ARMv9-A required: SVE2. Everything below carries every previous
    // requirement (the ARM ARM designates strict supersets).
    let v9 = has(flags, "sve2");

    // ARMv8.4-A: i8mm (matrix-multiply int8) + SHA3.
    let v8_4 = has(flags, "i8mm") && has(flags, "sha3");

    // ARMv8.2-A: FP16 + dot-product.
    // The kernel spells FP16 as `fphp` (float half-precision) and
    // dot-product as `asimddp`.
    let v8_2 = has(flags, "fphp") && has(flags, "asimddp");

    if v9 && v8_4 && v8_2 {
        AArch64Level::Armv9A
    } else if v8_4 && v8_2 {
        AArch64Level::Armv8_4A
    } else if v8_2 {
        AArch64Level::Armv8_2A
    } else {
        AArch64Level::Armv8A
    }
}

/// Given the host's detected level and the set of levels a package
/// advertises FOR THE SAME ARCH, return the highest available level
/// that fits. Publishers whose manifest declares no build for the
/// host arch cause the caller to refuse the install; this fn only
/// runs after `IsaRequirements::for_arch(host_arch)` returned `Some`.
///
/// Cross-arch mixing panics — the caller should have already filtered
/// by arch. Comparing an x86 level to an aarch64 level has no meaning.
pub fn pick_variant(host: IsaLevel, available: &[IsaLevel]) -> Option<IsaLevel> {
    let host_arch = host.arch();
    available
        .iter()
        .copied()
        .filter(|lvl| lvl.arch() == host_arch && lvl.rank() <= host.rank())
        .max_by_key(|lvl| lvl.rank())
}

/// Full install-time gate: look at the manifest's per-arch
/// requirements + the local CPU. Returns `Ok(level)` when there's a
/// build that fits, `Err(reason)` when there isn't (which the CLI
/// prints and refuses to install).
pub fn install_check(
    reqs: &IsaRequirements,
    host_arch: Arch,
    host_level: IsaLevel,
) -> Result<IsaLevel, InstallError> {
    let required = reqs
        .for_arch(host_arch)
        .ok_or(InstallError::NoBuildForArch(host_arch))?;
    if required.rank() > host_level.rank() {
        return Err(InstallError::HostTooOld {
            required,
            host: host_level,
        });
    }
    Ok(required)
}

/// Reasons the install-time ISA check refuses.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// The manifest declares no build for the host's arch (e.g. a
    /// package that's x86_64-only landing on an aarch64 device).
    NoBuildForArch(Arch),
    /// A build for this arch exists but demands a higher feature level
    /// than the local CPU exposes (e.g. requires v3 on a v2 CPU).
    HostTooOld { required: IsaLevel, host: IsaLevel },
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NoBuildForArch(arch) => {
                write!(f, "no build available for host architecture {arch:?}")
            }
            InstallError::HostTooOld { required, host } => write!(
                f,
                "package requires {} but this CPU supports at most {}",
                required.wire_label(),
                host.wire_label()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- x86_64 -----------------------------------------------------------

    #[test]
    fn x86_baseline_when_no_extras() {
        assert_eq!(detect_x86_64(""), X86_64Level::Baseline);
    }

    #[test]
    fn x86_v2_only() {
        assert_eq!(
            detect_x86_64("sse3 ssse3 sse4_1 sse4_2 popcnt cx16"),
            X86_64Level::V2
        );
    }

    #[test]
    fn x86_v3_full_set() {
        let flags = "sse3 ssse3 sse4_1 sse4_2 popcnt cx16 \
                     avx avx2 bmi1 bmi2 f16c fma abm movbe xsave";
        assert_eq!(detect_x86_64(flags), X86_64Level::V3);
    }

    #[test]
    fn x86_v4_full_set() {
        let flags = "sse3 ssse3 sse4_1 sse4_2 popcnt cx16 \
                     avx avx2 bmi1 bmi2 f16c fma abm movbe xsave \
                     avx512f avx512bw avx512cd avx512dq avx512vl";
        assert_eq!(detect_x86_64(flags), X86_64Level::V4);
    }

    #[test]
    fn x86_v3_missing_bmi2_falls_back_to_v2() {
        let flags = "sse3 ssse3 sse4_1 sse4_2 popcnt cx16 \
                     avx avx2 bmi1 f16c fma abm movbe xsave";
        assert_eq!(detect_x86_64(flags), X86_64Level::V2);
    }

    // ---- aarch64 ---------------------------------------------------------

    #[test]
    fn arm_v8_baseline() {
        // Neither fp16 nor asimddp — plain v8-A.
        assert_eq!(
            detect_aarch64("fp asimd aes sha1 sha2"),
            AArch64Level::Armv8A
        );
    }

    #[test]
    fn arm_v8_2_needs_fphp_and_dp() {
        let flags = "fp asimd aes sha1 sha2 fphp asimddp";
        assert_eq!(detect_aarch64(flags), AArch64Level::Armv8_2A);
    }

    #[test]
    fn arm_v8_4_needs_i8mm_and_sha3() {
        let flags = "fp asimd aes sha1 sha2 fphp asimddp i8mm sha3";
        assert_eq!(detect_aarch64(flags), AArch64Level::Armv8_4A);
    }

    #[test]
    fn arm_v9_needs_sve2() {
        let flags = "fp asimd aes sha1 sha2 fphp asimddp i8mm sha3 sve sve2";
        assert_eq!(detect_aarch64(flags), AArch64Level::Armv9A);
    }

    #[test]
    fn arm_missing_asimddp_drops_below_v8_2() {
        // fphp alone is not enough — v8.2-A requires dot-product too.
        assert_eq!(
            detect_aarch64("fp asimd aes sha1 sha2 fphp"),
            AArch64Level::Armv8A
        );
    }

    // ---- cpuinfo integration --------------------------------------------

    #[test]
    fn cpuinfo_x86_flags_line() {
        let c = "processor : 0\nvendor_id : GenuineIntel\n\
                 flags : fpu vme sse2 sse3 ssse3 sse4_1 sse4_2 popcnt cx16 \
                 avx avx2 bmi1 bmi2 f16c fma abm movbe xsave\n";
        assert_eq!(
            detect_from_cpuinfo(c, Arch::X86_64),
            IsaLevel::X86_64(X86_64Level::V3)
        );
    }

    #[test]
    fn cpuinfo_aarch64_features_line() {
        let c = "processor : 0\n\
                 Features : fp asimd aes sha1 sha2 fphp asimddp i8mm sha3\n";
        assert_eq!(
            detect_from_cpuinfo(c, Arch::AArch64),
            IsaLevel::AArch64(AArch64Level::Armv8_4A)
        );
    }

    // ---- pick_variant ----------------------------------------------------

    #[test]
    fn pick_returns_highest_that_fits() {
        let host = IsaLevel::X86_64(X86_64Level::V3);
        let available = [
            IsaLevel::X86_64(X86_64Level::Baseline),
            IsaLevel::X86_64(X86_64Level::V2),
            IsaLevel::X86_64(X86_64Level::V3),
        ];
        assert_eq!(
            pick_variant(host, &available),
            Some(IsaLevel::X86_64(X86_64Level::V3))
        );
    }

    #[test]
    fn pick_skips_variants_too_new() {
        let host = IsaLevel::X86_64(X86_64Level::V3);
        let available = [
            IsaLevel::X86_64(X86_64Level::V3),
            IsaLevel::X86_64(X86_64Level::V4),
        ];
        assert_eq!(
            pick_variant(host, &available),
            Some(IsaLevel::X86_64(X86_64Level::V3))
        );
    }

    #[test]
    fn pick_returns_none_when_arch_mismatched() {
        // Publisher only shipped aarch64 builds; host is x86.
        let host = IsaLevel::X86_64(X86_64Level::V3);
        let available = [IsaLevel::AArch64(AArch64Level::Armv8_2A)];
        assert_eq!(pick_variant(host, &available), None);
    }

    #[test]
    fn pick_returns_none_when_all_too_new() {
        let host = IsaLevel::X86_64(X86_64Level::Baseline);
        let available = [
            IsaLevel::X86_64(X86_64Level::V3),
            IsaLevel::X86_64(X86_64Level::V4),
        ];
        assert_eq!(pick_variant(host, &available), None);
    }

    // ---- install_check gate ---------------------------------------------

    #[test]
    fn install_check_ok_when_host_fits() {
        let reqs = IsaRequirements {
            x86_64: Some(X86_64Level::V3),
            aarch64: None,
        };
        let host = IsaLevel::X86_64(X86_64Level::V3);
        assert_eq!(
            install_check(&reqs, Arch::X86_64, host),
            Ok(IsaLevel::X86_64(X86_64Level::V3))
        );
    }

    #[test]
    fn install_check_refuses_missing_arch() {
        // Publisher shipped x86_64 only; host is aarch64.
        let reqs = IsaRequirements {
            x86_64: Some(X86_64Level::V3),
            aarch64: None,
        };
        let host = IsaLevel::AArch64(AArch64Level::Armv8_2A);
        assert_eq!(
            install_check(&reqs, Arch::AArch64, host),
            Err(InstallError::NoBuildForArch(Arch::AArch64))
        );
    }

    #[test]
    fn install_check_refuses_when_host_too_old() {
        // Publisher demands v3; host is v2.
        let reqs = IsaRequirements {
            x86_64: Some(X86_64Level::V3),
            aarch64: None,
        };
        let host = IsaLevel::X86_64(X86_64Level::V2);
        let got = install_check(&reqs, Arch::X86_64, host);
        assert!(matches!(got, Err(InstallError::HostTooOld { .. })));
    }
}
