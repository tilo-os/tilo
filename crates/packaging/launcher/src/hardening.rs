//! Which processes get the expensive runtime protections.
//!
//! Most of Tilo is Rust, which does not produce the bug classes a hardening
//! allocator catches, so paying for one everywhere would be a tax on
//! already-memory-safe code. Measured on this machine, an allocation-heavy loop
//! runs about 3.4x slower under scudo than under glibc's allocator (32 ms vs
//! 110 ms for three million short-lived allocations). That number is the reason
//! this file exists: full strength where the bugs live, nothing where they
//! don't.
//!
//! "Where the bugs live" means a process that parses bytes it did not produce,
//! in C or C++: a web engine, a media decoder, an image codec, a font shaper.
//! The list is **owned by the image, not by the package** — an app cannot opt
//! out, since the decision is about how much the system trusts the app's
//! inputs. Extended at image build time via [`POLICY_PATH`]; the compiled-in
//! list is the fallback so a missing file degrades to the known-risky set.
//!
//! Two protections: **hardened allocator** (scudo, from compiler-rt) —
//! out-of-line metadata, guard slabs, header checksums, delayed reuse, turning
//! a heap overflow or double free into a controlled abort — and **hardware
//! shadow stacks** (`GLIBC_TUNABLES=glibc.cpu.hwcaps=SHSTK`): every Tilo binary
//! is link-time compatible, but glibc won't switch shadow stacks on without
//! this opt-in, which scopes the protection instead of making it global and
//! is inert (not an error) on a CPU without shadow-stack support.
//!
//! Inert only at startup: once shadow stacks are on, glibc refuses to
//! `dlopen` any object without the SHSTK note, and a multithreaded process
//! cannot drop them again (`glibc.cpu.x86_shstk=permissive` does not help).
//! GPU drivers are dlopened, so every library in the pinned driver's closure
//! must be marked. Fedora 44's libzstd is not (its decoder assembly carries no
//! GNU property note) and Mesa's RADV needs it: on a shadow-stack CPU the
//! Vulkan loader finds no driver, wgpu has no backend, and the app never draws
//! a window. An image that launches these packages must ship a marked libzstd
//! (zstd built with `ZSTD_DISABLE_ASM` and `-fcf-protection`; the hosted guest
//! image does this) or otherwise check the closure of the drivers it ships.

use std::path::{Path, PathBuf};

/// Image-owned list of package ids whose processes parse untrusted
/// bytes. One entry per line, `#` for a comment, a trailing `*` for a
/// prefix match (`so.tilo.*` covers a family).
pub const POLICY_PATH: &str = "/usr/lib/tilo/untrusted-parsers.conf";

/// Fallback when [`POLICY_PATH`] is absent — the packages in this tree
/// that decode attacker-supplied data through a C or C++ library:
/// browser (a whole web engine, the only package allowed to generate
/// code), tube/music (container demux and audio/video decode operating
/// on bytes from the network).
pub const BUILTIN_HIGH_RISK: &[&str] = &["so.tilo.browser", "so.tilo.tube", "so.tilo.music"];

/// Environment variable that overrides [`POLICY_PATH`] — for tests to
/// point at a fixture; production reads the image's file. Honoured only by
/// developer builds (see [`crate::dev_env`]).
pub const POLICY_PATH_ENV: &str = "TILO_UNTRUSTED_PARSERS_FILE";

/// Image-owned Scudo copy. Tilo retains this DSO without shipping the complete
/// compiler-rt sanitizer/toolchain payload it came from.
pub const TILO_SCUDO_PATH: &str = "/usr/lib/tilo/libclang_rt.scudo_standalone.so";

/// glibc tunables that keep transparent huge pages out of malloc arenas.
/// Fedora's glibc asks the kernel for them by default, which makes every
/// thread's 2 MiB arena resident when it first allocates (an idle compositor
/// held 180 MiB for 34 MiB of live data). The session sets this for the
/// compositor and its services (`os/usr/libexec/tilo-session`, held to this
/// value by a test) and every sandboxed app gets it too.
///
/// Only settings that make glibc call less may go here, never more: this
/// environment reaches code with syscall policies of its own (Chromium's
/// renderers, every Electron app the person runs). `glibc.pthread.stack_hugetlb=0`
/// was here and broke exactly that: it makes `pthread_create` call
/// `madvise(MADV_NOHUGEPAGE)` on each new stack and fail when the call is
/// refused, and Chromium's renderer sandbox refuses it with `EPERM`. Every
/// renderer died on its first thread and the browser loaded no page. Thread
/// stacks need no opt-out on Tilo: the kernel gives huge pages only to memory
/// that asks for them (`CONFIG_TRANSPARENT_HUGEPAGE_MADVISE`), and nothing
/// asks for a stack.
pub const NO_HUGE_PAGES_TUNABLES: &str = "glibc.malloc.hugetlb=0";

/// `GLIBC_TUNABLES` for one launch: [`NO_HUGE_PAGES_TUNABLES`], plus the
/// shadow-stack request when the launch has it.
pub fn tunables(shadow_stack: bool) -> String {
    if shadow_stack {
        format!("glibc.cpu.hwcaps=SHSTK:{NO_HUGE_PAGES_TUNABLES}")
    } else {
        NO_HUGE_PAGES_TUNABLES.to_owned()
    }
}

/// The extra runtime protections to apply to one launch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeHardening {
    /// Absolute host path of the hardened allocator to `LD_PRELOAD`, when
    /// wanted and present. `None` means the system allocator — not on the
    /// list, or the image was built without compiler-rt, which must not be
    /// fatal.
    pub allocator: Option<PathBuf>,
    /// Ask glibc to enable hardware shadow stacks for this process.
    pub shadow_stack: bool,
}

impl RuntimeHardening {
    /// Nothing beyond the baseline sandbox.
    pub fn none() -> Self {
        Self::default()
    }

    /// True when this launch carries any of the extra protections.
    pub fn is_active(&self) -> bool {
        self.allocator.is_some() || self.shadow_stack
    }
}

/// Does `package_id` match one entry of a policy list?
///
/// Split out from file reading so matching is testable without a
/// filesystem: comments/blank lines ignored, a bare id is exact, a
/// trailing `*` is a prefix match.
pub fn policy_matches(list: &str, package_id: &str) -> bool {
    list.lines().any(|line| {
        let entry = line.split('#').next().unwrap_or("").trim();
        if entry.is_empty() {
            return false;
        }
        match entry.strip_suffix('*') {
            Some(prefix) => package_id.starts_with(prefix),
            None => entry == package_id,
        }
    })
}

/// Where the scudo standalone allocator lives, if installed.
///
/// The path carries clang's major version, so it moves with every
/// compiler-rt update and can't be hard-coded. Absence is `None`, not
/// an error — an image without compiler-rt should still launch apps,
/// with the system allocator.
pub fn find_hardened_allocator() -> Option<PathBuf> {
    const NAME: &str = "libclang_rt.scudo_standalone.so";
    let tilo_copy = PathBuf::from(TILO_SCUDO_PATH);
    if tilo_copy.is_file() {
        return Some(tilo_copy);
    }
    // Flat library dirs first — a distribution that ships the library
    // unversioned puts it here.
    for root in ["/usr/lib64", "/usr/lib"] {
        let direct = Path::new(root).join(NAME);
        if direct.is_file() {
            return Some(direct);
        }
    }
    // Fedora's layout is /usr/lib/clang/<major>/lib/<triple>/<name>.
    // Searching only the clang trees, not all of /usr/lib, keeps this a
    // handful of readdir calls on the launch path for every high-risk
    // package.
    for root in ["/usr/lib64/clang", "/usr/lib/clang"] {
        if let Some(found) = search_for(Path::new(root), NAME, 3) {
            return Some(found);
        }
    }
    None
}

/// Bounded-depth search, so a deep or symlink-looped tree can't turn a
/// launch into a filesystem walk.
fn search_for(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.is_symlink())
        .collect();
    // Deterministic order so two machines with the same layout resolve to
    // the same library, and a newer clang wins over an older one left behind.
    subdirs.sort();
    subdirs.reverse();
    subdirs
        .into_iter()
        .find_map(|sub| search_for(&sub, name, depth.saturating_sub(1)))
}

/// Decide the hardening for a package, reading the image's policy list.
pub fn for_package(package_id: &str) -> RuntimeHardening {
    let policy_path = package_store::dev_env::dev_override(POLICY_PATH_ENV)
        .map_or_else(|| PathBuf::from(POLICY_PATH), PathBuf::from);
    let high_risk = match std::fs::read_to_string(&policy_path) {
        Ok(list) => policy_matches(&list, package_id),
        // No policy file: fall back to the compiled-in set rather than to
        // "harden nothing".
        Err(_) => BUILTIN_HIGH_RISK.contains(&package_id),
    };
    if !high_risk {
        return RuntimeHardening::none();
    }
    RuntimeHardening {
        allocator: find_hardened_allocator(),
        shadow_stack: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_starts_the_compositor_and_its_services_without_huge_pages() {
        let session = include_str!("../../../../os/usr/libexec/tilo-session");
        assert!(
            session.contains(&format!(
                "\nexport GLIBC_TUNABLES={NO_HUGE_PAGES_TUNABLES}\n"
            )),
            "the session script must export the launcher's tunables to the compositor"
        );
        let imports: Vec<&str> = session
            .lines()
            .filter(|line| {
                line.contains("import-environment") || line.contains("$DBUS_UPDATE\" --systemd XDG")
            })
            .collect();
        assert!(
            imports.len() == 2 && imports.iter().all(|line| line.contains("GLIBC_TUNABLES")),
            "the user manager's services must get them too: {imports:?}"
        );
    }

    #[test]
    fn a_launch_carries_the_huge_page_opt_out_beside_shadow_stacks() {
        assert_eq!(tunables(false), NO_HUGE_PAGES_TUNABLES);
        assert_eq!(
            tunables(true),
            format!("glibc.cpu.hwcaps=SHSTK:{NO_HUGE_PAGES_TUNABLES}")
        );
    }

    /// Set in the re-executed test binary that runs the probe.
    #[cfg(all(target_os = "linux", target_endian = "little"))]
    const ENGINE_THREAD_PROBE: &str = "TILO_TEST_ENGINE_THREAD_PROBE";

    /// Refuse huge-page advice with `EPERM`, as Chromium's renderer sandbox
    /// does, then start a thread and report whether it started.
    #[cfg(all(target_os = "linux", target_endian = "little"))]
    fn start_a_thread_inside_an_engine_sandbox() -> bool {
        let word = |value: i64| u32::try_from(value).expect("a BPF immediate");
        // The low word of the third argument, `advice` (little-endian).
        let advice = std::mem::offset_of!(libc::seccomp_data, args)
            .checked_add(16)
            .expect("an offset inside seccomp_data");
        let statement = |code: u32, k: u32| libc::sock_filter {
            code: u16::try_from(code).expect("a BPF opcode"),
            jt: 0,
            jf: 0,
            k,
        };
        let jump = |k: u32, jt: u8, jf: u8| libc::sock_filter {
            jt,
            jf,
            ..statement(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, k)
        };
        let filter = [
            statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0),
            jump(word(libc::SYS_madvise), 0, 4),
            statement(
                libc::BPF_LD | libc::BPF_W | libc::BPF_ABS,
                u32::try_from(advice).expect("a field offset"),
            ),
            jump(word(libc::MADV_NOHUGEPAGE.into()), 1, 0),
            jump(word(libc::MADV_HUGEPAGE.into()), 0, 1),
            statement(
                libc::BPF_RET | libc::BPF_K,
                libc::SECCOMP_RET_ERRNO | word(libc::EPERM.into()),
            ),
            statement(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW),
        ];
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len()).expect("a short program"),
            filter: filter.as_ptr().cast_mut(),
        };
        // SAFETY: scalar prctl calls on this process; `program` points at
        // `filter`, which outlives the call, and the kernel copies it.
        unsafe {
            assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
            assert_eq!(
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const program
                ),
                0,
                "the probe's filter would not install"
            );
        }
        std::thread::Builder::new()
            .spawn(|| ())
            .is_ok_and(|thread| thread.join().is_ok())
    }

    /// Run the probe in a fresh process: glibc reads `GLIBC_TUNABLES` only
    /// when a process starts.
    #[cfg(all(target_os = "linux", target_endian = "little"))]
    fn thread_starts_under(tunables: &str) -> bool {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "hardening::tests::every_launch_environment_lets_a_web_engines_sandboxed_processes_start_threads",
                "--nocapture",
            ])
            .env(ENGINE_THREAD_PROBE, "1")
            .env("GLIBC_TUNABLES", tunables)
            .output()
            .expect("run the thread probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "the thread probe did not run: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if stdout.contains("engine thread: started") {
            true
        } else {
            assert!(stdout.contains("engine thread: refused"), "{stdout}");
            false
        }
    }

    /// The browser loaded no page: every Chromium renderer logged
    /// `pthread_create: Operation not permitted` and died, two thousand of
    /// them in fourteen seconds. The launch environment carried
    /// `glibc.pthread.stack_hugetlb=0`, which makes glibc advise every new
    /// thread stack against huge pages and fail the thread when the advice
    /// is refused, and Chromium's renderer sandbox refuses it. The tunables
    /// every launch and the session hand on must leave thread creation to
    /// what such a sandbox allows; the stack opt-out is the control that
    /// shows the probe sees the failure.
    #[test]
    #[cfg(all(target_os = "linux", target_endian = "little"))]
    fn every_launch_environment_lets_a_web_engines_sandboxed_processes_start_threads() {
        if std::env::var_os(ENGINE_THREAD_PROBE).is_some() {
            let started = start_a_thread_inside_an_engine_sandbox();
            println!(
                "engine thread: {}",
                if started { "started" } else { "refused" }
            );
            return;
        }
        for tunables in [tunables(false), tunables(true)] {
            assert!(
                thread_starts_under(&tunables),
                "GLIBC_TUNABLES={tunables} stops a sandboxed renderer from starting threads"
            );
        }
        assert!(
            !thread_starts_under("glibc.pthread.stack_hugetlb=0"),
            "the probe must see the refusal that killed Chromium's renderers"
        );
    }

    #[test]
    fn hardening_policy_entries_match_ids_and_families() {
        {
            let list = "so.tilo.browser\n";
            assert!(policy_matches(list, "so.tilo.browser"));
            assert!(!policy_matches(list, "so.tilo.browserish"));
            assert!(!policy_matches(list, "so.tilo.calculator"));
        }
        {
            let list = "\n# so.tilo.calculator\n   \nso.tilo.tube  # decodes video\n";
            assert!(policy_matches(list, "so.tilo.tube"));
            assert!(
                !policy_matches(list, "so.tilo.calculator"),
                "a commented-out id must not match"
            );
        }
        {
            let list = "so.tilo.media.*\n";
            assert!(policy_matches(list, "so.tilo.media.player"));
            assert!(!policy_matches(list, "so.tilo.notes"));
        }
        {
            assert!(!policy_matches("", "so.tilo.browser"));
            assert!(!policy_matches(
                "# everything commented\n",
                "so.tilo.browser"
            ));
        }
    }

    #[test]
    fn hardening_policy_file_replaces_the_builtin_list() {
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("untrusted-parsers.conf");
            std::fs::write(&file, "so.tilo.notes\n").unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(POLICY_PATH_ENV, &file);

            let notes = for_package("so.tilo.notes");
            let browser = for_package("so.tilo.browser");
            drop(env);

            assert!(
                notes.shadow_stack,
                "the package named by the policy file must be hardened"
            );
            assert!(
                !browser.is_active(),
                "a package the policy file omits must not be hardened just because \
                 it is in the compiled-in fallback"
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("does-not-exist.conf");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(POLICY_PATH_ENV, &missing);

            let browser = for_package("so.tilo.browser");
            let calculator = for_package("so.tilo.calculator");
            drop(env);

            assert!(browser.shadow_stack, "fallback list lost the browser");
            assert!(!calculator.is_active(), "a plain app must stay unhardened");
        }
    }
}
