//! Detect the local CPU's highest supported architecture-specific
//! feature level.
//!
//! Two-arch today (`x86_64`, aarch64); adding a new arch is a matching
//! extension to `tilo_app_package::manifest::Arch`, a new detector function here,
//! plus a `pick_variant` case.
//!
//! # How it works
//!
//! Reads CPU feature flags out of `/proc/cpuinfo` — the kernel already
//! parses CPUID (x86) / MIDR + `Features:` (aarch64) and exposes them
//! there. Prefer this over inline asm because it's testable (feed a
//! synthetic flags line and assert the mapping) and works from any
//! userland.

use std::fs;
use tilo_app_package::manifest::{AArch64Level, Arch, IsaLevel, IsaRequirements, X86_64Level};

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
        panic!("Tilo binary compiled for unsupported target arch");
    }
}

/// Public for tests + for the CLI's `tilo-install isa-report`.
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
///
/// # Errors
///
/// Returns [`InstallError::NoBuildForArch`] if the manifest declares
/// no build for `host_arch`, or [`InstallError::HostTooOld`] if the
/// build for that arch requires a higher ISA level than `host_level`.
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

    const X86_V2: &str = "sse3 ssse3 sse4_1 sse4_2 popcnt cx16";
    const X86_V3_EXTRA: &str = "avx avx2 bmi1 bmi2 f16c fma abm movbe xsave";
    const ARM_BASE: &str = "fp asimd aes sha1 sha2";

    #[test]
    fn cpu_flags_detect_the_highest_complete_isa_level() {
        use AArch64Level::{Armv8_2A, Armv8_4A, Armv8A, Armv9A};
        use X86_64Level::{Baseline, V2, V3, V4};
        let v3_without_bmi2 = format!("{X86_V2} avx avx2 bmi1 f16c fma abm movbe xsave");
        for (name, flags, want) in [
            ("x86 baseline when no extras", String::new(), Baseline),
            ("x86 v2 only", X86_V2.to_string(), V2),
            ("x86 v3 full set", format!("{X86_V2} {X86_V3_EXTRA}"), V3),
            (
                "x86 v4 full set",
                format!("{X86_V2} {X86_V3_EXTRA} avx512f avx512bw avx512cd avx512dq avx512vl"),
                V4,
            ),
            ("x86 v3 missing bmi2 falls back to v2", v3_without_bmi2, V2),
        ] {
            assert_eq!(detect_x86_64(&flags), want, "case {name}");
        }
        for (name, extra, want) in [
            // Neither fp16 nor asimddp — plain v8-A.
            ("arm v8 baseline", "", Armv8A),
            ("arm v8.2 needs fphp and dp", " fphp asimddp", Armv8_2A),
            (
                "arm v8.4 needs i8mm and sha3",
                " fphp asimddp i8mm sha3",
                Armv8_4A,
            ),
            (
                "arm v9 needs sve2",
                " fphp asimddp i8mm sha3 sve sve2",
                Armv9A,
            ),
            // fphp alone is not enough — v8.2-A requires dot-product too.
            ("arm missing asimddp drops below v8.2", " fphp", Armv8A),
        ] {
            assert_eq!(
                detect_aarch64(&format!("{ARM_BASE}{extra}")),
                want,
                "case {name}"
            );
        }

        // The flags line is found inside a whole /proc/cpuinfo.
        let c = "processor : 0\nvendor_id : GenuineIntel\n\
                 flags : fpu vme sse2 sse3 ssse3 sse4_1 sse4_2 popcnt cx16 \
                 avx avx2 bmi1 bmi2 f16c fma abm movbe xsave\n";
        assert_eq!(detect_from_cpuinfo(c, Arch::X86_64), IsaLevel::X86_64(V3));
        let c = "processor : 0\n\
                 Features : fp asimd aes sha1 sha2 fphp asimddp i8mm sha3\n";
        assert_eq!(
            detect_from_cpuinfo(c, Arch::AArch64),
            IsaLevel::AArch64(Armv8_4A)
        );
    }

    #[test]
    fn variant_choice_and_install_check_respect_the_host_level() {
        use X86_64Level::{Baseline, V2, V3, V4};
        let x86 = IsaLevel::X86_64;
        for (name, host, available, want) in [
            (
                "pick returns highest that fits",
                x86(V3),
                vec![x86(Baseline), x86(V2), x86(V3)],
                Some(x86(V3)),
            ),
            (
                "pick skips variants too new",
                x86(V3),
                vec![x86(V3), x86(V4)],
                Some(x86(V3)),
            ),
            // Publisher only shipped aarch64 builds; host is x86.
            (
                "pick returns none when arch mismatched",
                x86(V3),
                vec![IsaLevel::AArch64(AArch64Level::Armv8_2A)],
                None,
            ),
            (
                "pick returns none when all too new",
                x86(Baseline),
                vec![x86(V3), x86(V4)],
                None,
            ),
        ] {
            assert_eq!(pick_variant(host, &available), want, "case {name}");
        }

        // The publisher shipped x86_64 v3 only.
        let reqs = IsaRequirements {
            x86_64: Some(V3),
            aarch64: None,
        };
        assert_eq!(install_check(&reqs, Arch::X86_64, x86(V3)), Ok(x86(V3)));
        assert_eq!(
            install_check(
                &reqs,
                Arch::AArch64,
                IsaLevel::AArch64(AArch64Level::Armv8_2A)
            ),
            Err(InstallError::NoBuildForArch(Arch::AArch64))
        );
        assert!(matches!(
            install_check(&reqs, Arch::X86_64, x86(V2)),
            Err(InstallError::HostTooOld { .. })
        ));
    }
}
