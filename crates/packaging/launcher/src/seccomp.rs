//! Baseline seccomp-bpf policy, as the launcher applies it.
//!
//! The filter itself — Flatpak-derived deny-list, namespace-flag rules on
//! `clone`, write-xor-execute — is compiled by `sandbox_core::seccomp`,
//! the one implementation every Tilo confinement path shares. What is
//! decided here is which profile a package gets: the manifest names one,
//! and the image's entitlement list decides whether the package may have
//! it ([`resolve_profile`]).
//!
//! At runtime the launcher compiles the filter into a BPF blob, writes
//! it to a temp file, and passes the open fd to `bwrap`'s `--seccomp
//! <fd>`. [`compile_baseline`] is the default fast path.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub use sandbox_core::seccomp::{
    SeccompError, compile_app_baseline, compile_baseline, compile_clone3_compat, write_baseline_to,
};
pub use sandbox_core::seccomp::{SeccompProfile, SyscallGrants};

/// The manifest capability that lets an app use `io_uring`
/// (`sandbox_core::seccomp::IO_URING_SYSCALLS`).
pub const IO_URING: &str = "io-uring";

/// The syscalls one launch of `manifest` may make beyond the baseline.
///
/// `io_uring` is a permission, decided like the system information grants
/// (`crate::system_stats`): the manifest is the ceiling (an app that does
/// not declare `io-uring` never gets it); an app the image ships
/// (`image_shipped`) is granted what it declares; any other app only what
/// the room's capabilities name (`Scope::Device("io-uring")`), which the
/// person gives through the permission prompt. Without it the calls fail
/// with `EPERM` and the app's runtime falls back to `epoll`.
#[must_use]
pub fn syscall_grants(
    manifest: &crate::manifest::ManifestView,
    plan: &crate::capabilities::CapabilityPlan,
    image_shipped: bool,
) -> SyscallGrants {
    let declared = manifest.declares_capability(IO_URING);
    let granted = image_shipped || plan.devices.iter().any(|device| device == IO_URING);
    if declared && !granted {
        eprintln!(
            "tilo-launcher: {} declares {IO_URING} but holds no grant for it in this room; \
             io_uring is refused (EPERM) and the app falls back to epoll",
            manifest.package_id
        );
    }
    SyscallGrants {
        io_uring: declared && granted,
    }
}

/// Image-owned list of package ids that may run without
/// write-xor-execute.
///
/// One entry per line, `#` starts a comment, a trailing `*` is a prefix
/// match — same format as [`crate::hardening::POLICY_PATH`], so the two
/// image-owned policies cannot drift into different syntaxes.
pub const JIT_POLICY_PATH: &str = "/usr/lib/tilo/jit-entitled.conf";

/// Overrides [`JIT_POLICY_PATH`], so a test can point at a fixture. Honoured
/// only by developer builds (see [`crate::dev_env`]): the file grants
/// exemption from write-xor-execute.
pub const JIT_POLICY_PATH_ENV: &str = "TILO_JIT_ENTITLED_FILE";

/// Fallback when [`JIT_POLICY_PATH`] is absent — the packages in this
/// tree that genuinely compile code at run time.
///
/// One entry, and it should stay that way: the browser's JavaScript
/// tier emits machine code it then executes; anything else almost
/// certainly wants a write-then-flip JIT, which the strict profile
/// already allows.
pub const BUILTIN_JIT_ENTITLED: &[&str] = &["so.tilo.browser"];

/// Whether the image lets `package_id` run without write-xor-execute.
///
/// Same shape as [`crate::hardening::for_package`], for the same
/// reason: a missing file must degrade to the known set, not to
/// "nobody" — which would leave the browser unable to render on any
/// image that didn't ship the file.
pub fn entitled_to_dynamic_code(package_id: &str) -> bool {
    let policy_path = package_store::dev_env::dev_override(JIT_POLICY_PATH_ENV)
        .map_or_else(|| PathBuf::from(JIT_POLICY_PATH), PathBuf::from);
    match std::fs::read_to_string(&policy_path) {
        Ok(list) => crate::hardening::policy_matches(&list, package_id),
        Err(_) => BUILTIN_JIT_ENTITLED.contains(&package_id),
    }
}

/// Image-owned list of package ids that may build their own nested sandbox
/// ([`SeccompProfile::TiloWebEngine`]). Same format as [`JIT_POLICY_PATH`].
pub const WEB_ENGINE_POLICY_PATH: &str = "/usr/lib/tilo/web-engine-entitled.conf";

/// Overrides [`WEB_ENGINE_POLICY_PATH`] in developer builds, as
/// [`JIT_POLICY_PATH_ENV`] does.
pub const WEB_ENGINE_POLICY_PATH_ENV: &str = "TILO_WEB_ENGINE_ENTITLED_FILE";

/// Fallback when [`WEB_ENGINE_POLICY_PATH`] is absent: the browser, whose
/// Chromium engine refuses to run renderers without its own sandbox.
pub const BUILTIN_WEB_ENGINE_ENTITLED: &[&str] = &["so.tilo.browser"];

/// Whether the image lets `package_id` create the namespaces of a web
/// engine's renderer sandbox.
pub fn entitled_to_nested_sandbox(package_id: &str) -> bool {
    let policy_path = package_store::dev_env::dev_override(WEB_ENGINE_POLICY_PATH_ENV)
        .map_or_else(|| PathBuf::from(WEB_ENGINE_POLICY_PATH), PathBuf::from);
    match std::fs::read_to_string(&policy_path) {
        Ok(list) => crate::hardening::policy_matches(&list, package_id),
        Err(_) => BUILTIN_WEB_ENGINE_ENTITLED.contains(&package_id),
    }
}

/// What a manifest asked for, and what it is actually going to get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDecision {
    /// The profile that will be compiled and installed.
    pub applied: SeccompProfile,
    /// The name the manifest wrote, verbatim.
    pub requested: String,
    /// Set when the manifest named a real, weaker profile it is not
    /// entitled to — as distinct from a typo, which [`SeccompProfile::parse`]
    /// already resolves strict.
    pub refused: bool,
}

/// Decide the syscall profile for one launch.
///
/// The manifest names a profile and the **image** decides whether this package
/// may have it. A `.tapp` carries its own manifest, so "the package asked for
/// it" is not an authority — it is the applicant filling in their own permit,
/// and write-xor-execute was the one protection a package could switch off by
/// writing a string about itself. Runtime hardening is already decided this
/// way; this is the same rule applied to the syscall filter.
///
/// Fail-closed twice over: an unrecognised name resolves strict, and a
/// recognised weaker name resolves strict too unless the image says this
/// package id may have it.
pub fn resolve_profile(requested: &str, package_id: &str) -> ProfileDecision {
    let asked = SeccompProfile::parse(requested);
    let dynamic_code = !asked.allows_dynamic_code() || entitled_to_dynamic_code(package_id);
    let nested_sandbox = !asked.allows_nested_sandbox() || entitled_to_nested_sandbox(package_id);
    let applied = match (dynamic_code, nested_sandbox) {
        (true, true) => asked,
        // Refused one permission of two: keep the one the image granted.
        (true, false) => SeccompProfile::TiloJit,
        (false, _) => SeccompProfile::TiloDefault,
    };
    ProfileDecision {
        applied,
        requested: requested.to_string(),
        refused: applied != asked,
    }
}

/// The syscall profile a manifest's sandbox gets, resolved fail-closed
/// against the image's entitlement policy. Takes the package id since the
/// manifest's own answer isn't sufficient: whether a package may run
/// without write-xor-execute is the image's decision, not the packager's.
pub fn manifest_profile(sandbox: &crate::manifest::Sandbox, package_id: &str) -> SeccompProfile {
    resolve_profile(&sandbox.seccomp, package_id).applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The compiled program, byte for byte, is the policy. Pinning it means
    /// any change to what a launched app may call — a rule added, dropped,
    /// reordered or recompiled differently — fails here and has to be
    /// reviewed as a policy change together with a regenerated golden file,
    /// instead of passing silently behind a refactor.
    ///
    /// Only the `x86_64` program is pinned: syscall numbers and the arch
    /// check are per-architecture, so each arch would need its own file.
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn compiled_baseline_programs_match_the_pinned_golden_files() {
        let cases: [(SeccompProfile, &str); 3] = [
            (
                SeccompProfile::TiloDefault,
                include_str!("../tests/fixtures/seccomp/tilo-default.x86_64.bpf.hex"),
            ),
            (
                SeccompProfile::TiloJit,
                include_str!("../tests/fixtures/seccomp/tilo-jit.x86_64.bpf.hex"),
            ),
            (
                SeccompProfile::TiloWebEngine,
                include_str!("../tests/fixtures/seccomp/tilo-web-engine.x86_64.bpf.hex"),
            ),
        ];
        for (profile, golden_hex) in cases {
            let golden = hex::decode(golden_hex.lines().collect::<String>())
                .expect("decode pinned BPF hex fixture");
            let compiled = compile_baseline(profile).expect("compile");
            assert!(
                compiled == golden,
                "{} compiled to a different BPF program than the pinned one \
                 ({} bytes vs {} pinned); this is a sandbox policy change",
                profile.as_str(),
                compiled.len(),
                golden.len()
            );
        }
    }

    /// `io_uring` reaches an app only when its manifest declares it and the
    /// image ships the app or the room granted it.
    #[test]
    fn io_uring_needs_the_declaration_and_a_grant() {
        use crate::capabilities::CapabilityPlan;
        let mut manifest = crate::manifest::test_manifest(
            "id = \"com.example.storage\"\nversion = \"1.0.0\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/app\"\n",
        );
        let granted = CapabilityPlan {
            devices: vec![IO_URING.to_string()],
            ..CapabilityPlan::empty()
        };
        let none = CapabilityPlan::empty();

        // Undeclared: never, whatever the room or the image says.
        assert!(!syscall_grants(&manifest, &granted, true).io_uring);

        manifest.capabilities = vec![IO_URING.to_string()];
        assert!(
            !syscall_grants(&manifest, &none, false).io_uring,
            "a user-installed app without the room's grant is refused"
        );
        assert!(syscall_grants(&manifest, &granted, false).io_uring);
        assert!(
            syscall_grants(&manifest, &none, true).io_uring,
            "an app the image ships gets what it declares"
        );
    }

    /// Points the entitlement lookup at a fixture for the duration of a test.
    struct JitPolicy {
        _env: tilo_test_env::ScopedEnv,
        _dir: tempfile::TempDir,
    }

    impl JitPolicy {
        fn listing(entries: &str) -> Self {
            Self::lists(entries, entries)
        }

        /// Separate JIT and nested-sandbox entitlement lists.
        fn lists(jit: &str, web_engine: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            for (name, entries, variable) in [
                ("jit-entitled.conf", jit, JIT_POLICY_PATH_ENV),
                (
                    "web-engine-entitled.conf",
                    web_engine,
                    WEB_ENGINE_POLICY_PATH_ENV,
                ),
            ] {
                let file = dir.path().join(name);
                std::fs::write(&file, entries).unwrap();
                env.set(variable, &file);
            }
            Self {
                _env: env,
                _dir: dir,
            }
        }

        /// A path that does not exist, so the compiled-in fallback decides.
        fn missing() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(JIT_POLICY_PATH_ENV, dir.path().join("does-not-exist.conf"));
            env.set(
                WEB_ENGINE_POLICY_PATH_ENV,
                dir.path().join("does-not-exist.conf"),
            );
            Self {
                _env: env,
                _dir: dir,
            }
        }
    }

    #[test]
    fn seccomp_profile_needs_both_request_and_entitlement() {
        // Phase `a_package_the_image_does_not_entitle_gets_the_strict_profile`.
        // A package cannot lift write-xor-execute off itself by saying so: a
        // `.tapp` carries its own manifest, so naming `tilo-jit` is the applicant
        // filling in their own permit.
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");

            let decision = resolve_profile("tilo-jit", "com.example.evil");

            assert_eq!(
                decision.applied,
                SeccompProfile::TiloDefault,
                "a manifest asking for tilo-jit without entitlement must be confined \
                 by the strict profile"
            );
            assert!(
                decision.refused,
                "the launcher has to be able to say a request was refused, or the \
                 log cannot tell a typo from an app trying to weaken itself"
            );
            assert_eq!(decision.requested, "tilo-jit");
        }
        // Phase `the_package_the_image_entitles_still_gets_what_it_asked_for`.
        // Entitlement is a gate, not a ban: a rule that broke the browser's
        // JavaScript tier would take it off the system rather than harden it.
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");

            let decision = resolve_profile("tilo-jit", "so.tilo.browser");

            assert_eq!(decision.applied, SeccompProfile::TiloJit);
            assert!(!decision.refused);
        }
        // Phase `entitlement_without_a_request_changes_nothing`.
        // The list is a ceiling, not a setting: both sides have to agree.
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");

            let decision = resolve_profile("tilo-default", "so.tilo.browser");

            assert_eq!(
                decision.applied,
                SeccompProfile::TiloDefault,
                "being on the list must not weaken a package that did not ask"
            );
            assert!(!decision.refused);
        }
        // Phase `a_misspelt_profile_resolves_strict_for_everyone`.
        // An entitled package must not reach a weaker profile by misspelling one.
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");

            for id in ["so.tilo.browser", "com.example.evil"] {
                for name in ["tilo-jitt", "TILO-JIT", "", "anything"] {
                    let decision = resolve_profile(name, id);
                    assert_eq!(
                        decision.applied,
                        SeccompProfile::TiloDefault,
                        "{id} asking for {name:?} must not reach a weaker profile"
                    );
                    assert!(
                        !decision.refused,
                        "{name:?} is an unrecognised name, not a refused \
                         entitlement; the two read differently in the log"
                    );
                }
            }
        }
        // Phase `the_manifest_accessor_consults_the_entitlement_policy`.
        // `manifest_profile` must not be a way around [`resolve_profile`].
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");
            let sandbox = crate::manifest::Sandbox {
                seccomp: "tilo-jit".to_string(),
                ..crate::manifest::Sandbox::default()
            };

            assert_eq!(
                manifest_profile(&sandbox, "com.example.evil"),
                SeccompProfile::TiloDefault,
                "the manifest accessor must not hand out what the image refused"
            );
            assert_eq!(
                manifest_profile(&sandbox, "so.tilo.browser"),
                SeccompProfile::TiloJit
            );
            assert_eq!(
                crate::manifest::Sandbox::default().seccomp,
                SeccompProfile::TiloDefault.as_str(),
                "the view's default profile is the launcher's strict one"
            );
        }
    }

    /// The refusal reaches the kernel, not just the enum: a test comparing
    /// two enum values would pass on a launcher that resolved strictly and
    /// compiled the other one.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_unentitled_request_is_refused_by_the_running_filter() {
        let _policy = JitPolicy::listing("so.tilo.browser\n");

        let applied = resolve_profile("tilo-jit", "com.example.evil").applied;
        assert!(
            !applied.allows_dynamic_code(),
            "fixture sanity: the resolved profile must be the strict one, or \
             the probe below proves nothing"
        );
        assert_eq!(
            sandbox_core::seccomp::testing::probe_mappings_under(applied),
            0,
            "10 = W+X was allowed, 11 = an ordinary mapping was refused, \
             12 = the filter would not install"
        );
    }

    /// A nested sandbox is its own entitlement: the browser's Chromium engine
    /// gets it, a package that names it for itself does not, and an image that
    /// entitles only the JIT keeps a web engine rendering without it.
    #[test]
    fn web_engine_profile_needs_its_own_entitlement() {
        // Phase `the_entitled_browser_gets_its_nested_sandbox`.
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");
            let decision = resolve_profile("tilo-web-engine", "so.tilo.browser");
            assert_eq!(decision.applied, SeccompProfile::TiloWebEngine);
            assert!(!decision.refused);
        }
        // Phase `an_unentitled_package_is_confined_strictly`.
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");
            let decision = resolve_profile("tilo-web-engine", "com.example.evil");
            assert_eq!(decision.applied, SeccompProfile::TiloDefault);
            assert!(decision.refused);
        }
        // Phase `jit_without_nested_sandbox_keeps_the_jit`.
        {
            let _policy = JitPolicy::lists("so.tilo.browser\n", "# nobody\n");
            let decision = resolve_profile("tilo-web-engine", "so.tilo.browser");
            assert_eq!(
                decision.applied,
                SeccompProfile::TiloJit,
                "refusing the nested sandbox must not also take the JIT away"
            );
            assert!(decision.refused);
        }
        // Phase `nested_sandbox_without_jit_is_strict`.
        {
            let _policy = JitPolicy::lists("# nobody\n", "so.tilo.browser\n");
            assert_eq!(
                resolve_profile("tilo-web-engine", "so.tilo.browser").applied,
                SeccompProfile::TiloDefault
            );
        }
        // Phase `the_fallback_entitles_the_browser_only`.
        {
            let _policy = JitPolicy::missing();
            assert_eq!(
                resolve_profile("tilo-web-engine", "so.tilo.browser").applied,
                SeccompProfile::TiloWebEngine
            );
            assert_eq!(
                resolve_profile("tilo-web-engine", "com.example.evil").applied,
                SeccompProfile::TiloDefault
            );
        }
        // Phase `the_running_filter_follows_the_decision`.
        #[cfg(target_os = "linux")]
        {
            let _policy = JitPolicy::listing("so.tilo.browser\n");
            for id in ["so.tilo.browser", "com.example.evil"] {
                let applied = resolve_profile("tilo-web-engine", id).applied;
                assert_eq!(
                    sandbox_core::seccomp::testing::probe_namespaces_under(applied),
                    0,
                    "{id}: 20 = a sandbox namespace got the wrong answer, 21 = another \
                     namespace kind was allowed, 22 = chroot got the wrong answer"
                );
            }
        }
        // Phase `the_shipped_list_and_the_builtin_list_agree`.
        {
            let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../os/usr/lib/tilo/web-engine-entitled.conf");
            let shipped = std::fs::read_to_string(&repo).unwrap_or_else(|e| {
                panic!(
                    "the image's list must be in the tree at {}: {e}",
                    repo.display()
                )
            });
            for id in BUILTIN_WEB_ENGINE_ENTITLED {
                assert!(crate::hardening::policy_matches(&shipped, id), "{id}");
            }
            assert!(!crate::hardening::policy_matches(
                &shipped,
                "com.example.evil"
            ));
        }
    }

    #[test]
    fn seccomp_entitlement_policy_file_replaces_the_builtin_list() {
        // Phase `a_policy_file_replaces_the_builtin_list`.
        // A present file replaces the fallback rather than adding to it.
        {
            let _policy = JitPolicy::listing("# nobody\n");

            assert_eq!(
                resolve_profile("tilo-jit", "so.tilo.browser").applied,
                SeccompProfile::TiloDefault,
                "the image's own file decides; the fallback must not add to it"
            );
        }
        // Phase `with_no_policy_file_the_builtin_entitlement_still_applies`.
        // A missing file must degrade to "the browser renders pages" rather than
        // to "nothing may generate code".
        {
            let _policy = JitPolicy::missing();

            assert_eq!(
                resolve_profile("tilo-jit", "so.tilo.browser").applied,
                SeccompProfile::TiloJit,
                "fallback list lost the browser"
            );
            assert_eq!(
                resolve_profile("tilo-jit", "com.example.evil").applied,
                SeccompProfile::TiloDefault,
                "the fallback must entitle the browser and nobody else"
            );
        }
        // Phase `the_shipped_policy_file_and_the_builtin_list_agree`.
        // Two lists that can disagree is a machine whose confinement depends
        // on whether a file was copied — a browser that behaves differently
        // on one image than another.
        {
            let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../os/usr/lib/tilo/jit-entitled.conf");
            let shipped = std::fs::read_to_string(&repo).unwrap_or_else(|e| {
                panic!(
                    "the image's entitlement list must be in the tree at {}: {e}",
                    repo.display()
                )
            });
            for id in BUILTIN_JIT_ENTITLED {
                assert!(
                    crate::hardening::policy_matches(&shipped, id),
                    "{id} is in the compiled-in fallback but not in the file the \
                     image ships"
                );
            }
            assert!(
                !crate::hardening::policy_matches(&shipped, "com.example.evil"),
                "the shipped list must not entitle an arbitrary package"
            );
        }
    }
}
