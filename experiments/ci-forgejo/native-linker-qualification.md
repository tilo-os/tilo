# Native Linux linker qualification (2026-09-19)

Status: candidate only, no workflow default or builder image changed. Prefer
preinstalling mold in native Linux compilation builders, not lint/control images.
Do not replace `/usr/bin/ld`, kernel linkers, Windows or macOS defaults.

`tools/ci-native-mold.sh` is opt-in and fails closed unless the explicit Rust
target matches the native Linux architecture and gcc's target. Enable only the
corresponding Cargo target linker in an experimental builder/job, for example:

```sh
export TILO_CI_MOLD_TARGET=aarch64-unknown-linux-gnu
export TILO_CI_MOLD_THREADS=2
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$PWD/tools/ci-native-mold.sh"
# Keep the repository's existing Cargo target rustflags; do not replace them.
```

Do not overwrite an existing target linker without checking why it is set. The
wrapper is for native GNU builds, not musl/cross compilation. It is not installed
as `cc` or selected for native C/C++ builds. No Cargo target directory changes.
Thread limit is per linker invocation, not a global Cargo CPU/memory limit.

## Local ARM64 smoke evidence

Native aarch64 Fedora WSL, not the immutable CI builder. Explicit Rust 1.97.1
(8bab26f4f), mold 2.40.4, bundled Rust LLD 22.1.6. A scratch Rust executable parses
an argument, calculates a sum and prints it. Both dev (debuginfo=2) and optimized
Rust thinLTO (opt-level=2, codegen-units=4) execute correctly. Mold and LLD receive
two linker threads. All configurations include generic CPU, frame pointers,
full RELRO and `-Wl,-z,noexecstack`.

| Linker | Dev wall / peak RSS | ThinLTO wall / peak RSS |
| --- | --- | --- |
| BFD | 0.36 s / 119580 KiB | 2.94 s / 212168 KiB |
| mold | 0.18 s / 120756 KiB | 2.04 s / 214484 KiB |
| bundled LLD | 0.13 s / 120628 KiB | 1.91 s / 211784 KiB |

These are single tiny **whole compile plus link** measurements from GNU time,
not isolated linker times or representative workspace benchmarks. They cannot
rank mold versus LLD or predict the current large-job memory improvement.

`readelf` confirms all six executables have GNU_RELRO, BIND_NOW, PIE and a
non-executable RW GNU_STACK; runtime dependencies remain libc and libgcc_s.
`.comment` identifies mold 2.40.4 and LLD 22.1.6 in the corresponding binaries.
Mold's thinLTO binary also declares STATIC_TLS; no failure observed in this smoke.
The initial scratch run accidentally used the user's default Rust 1.94.1 after
changing directories; that result was discarded and the entire experiment
repeated with explicit `rustc +1.97.1` (the table above).

Rust `-C lto=thin` performs optimization in rustc and passed here; this does not
qualify C/C++ plugin LTO or `-C linker-plugin-lto`. Changing the ELF linker is not
a Rust ABI change, but this smoke does not prove application packaging compatibility.

## Rollout gates

1. Record/pin mold package and builder digest; run this probe inside that exact
   native ARM64 and x86-64 builder (x86 also retains `-z shstk`).
2. On owner-approved completed build inputs, measure the same representative
   application/test link with BFD, mold and bundled LLD, recording link-only wall
   and peak RSS, bounded threads, and output ELF properties. Do not race a live
   Cargo target or trigger a full rebuild just to obtain input objects.
3. Run native application/test smoke and installed-package checks, including
   dynamic library dependencies and runtime version floors. Keep distro packaging
   policies, build IDs, debug symbol handling and sandbox settings intact.
4. Enable the matching native Cargo linker variable in a small CI canary first.
   Preserve hardening flags and support immediate rollback by unsetting that
   variable. A linker change may invalidate compiler cache keys once; record it.

Bundled LLD remains a viable no-extra-package alternative on the existing Rust
builder: use clang with `--ld-path=$(rustc --print sysroot)/lib/rustlib/<host>/bin/gcc-ld/ld.lld`.
The matching toolchain supplies this executable; no reliance on a system LLD
version is needed. Do not select a different toolchain when entering a scratch
directory.

The actual wrapper also linked and ran the native ARM64 Rust 1.97.1 scratch
program successfully: `.comment` identifies mold 2.40.4, with GNU_RELRO,
BIND_NOW and RW/non-executable GNU_STACK. Seven guard/argument tests passed,
including Fedora's actual `aarch64-redhat-linux` GCC tuple, cross-target refusal,
thread bounds and forwarding of filenames containing spaces and hardening flags.
The x86 forwarding test is synthetic, not native x86 qualification.
