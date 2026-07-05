# qOS Performance & Boot-Speed — Design

Status: **spec** — Clear-Linux-inspired cross-cutting performance work. Phased so
each layer lands independently and is measurable in isolation.

## 1. Purpose

Give qOS the perceived-instant boot + click-response Clear Linux was known for,
without buying Intel silicon, forking glibc, or losing our cross-time-reproducible
OS image contract. Every technique below is software engineering that applies to
the Ryzen 780H just as well as to Xeon Platinum.

Three tiers, applied at every layer:

1. **Compile-time**: every binary is LTO'd + PGO'd + BOLT'd + built to the right
   ISA level.
2. **Runtime dispatch**: shared libraries and multi-variant qOS-app packages let
   the actually-installed CPU pick the fastest safe binary at load / install time,
   with a userspace repair path when an aggressive variant SIGILLs.
3. **Structural boot cuts**: no initramfs on target hardware, `systemd-boot`
   instead of GRUB, ruthless service pruning, kernel config tuned for interactive
   throughput.

## 2. Non-goals

- **Forking glibc / GCC / clang.** Fedora ships them; we consume via the frozen
  RPM repo.
- **Retrofitting a stateless `/etc`.** Design decision to make separately; big
  invasive change with ecosystem consequences.
- **A separate package manager for legacy Linux apps.** Legacy stays on Flatpak
  (see `plan/architecture.md`); qOS apps continue through `qos-install` (which IS
  our package manager for qOS apps — the multi-ISA distribution work in §7 lives
  entirely inside it).
- **Per-CPU-model tuning.** We target ISA feature levels
  (`x86-64-v2`/`v3`/`v4`), not vendors.
- **Weakening CPU mitigations for throughput.** The security hardening on `main`
  explicitly ruled this out and we honour it.

## 3. Frozen-repo compatibility

Every optimization stays cross-time reproducible under the pinned RPM snapshot
(`os/rpms-snapshot.env`). Concretely:

- PGO profile bytes commit into `os/pgo/*.profdata` — the second-phase compile
  reads them from the repo, so same source + same profile = same output.
- Toolchain versions stay pinned via the frozen repo (Rust, cargo, sccache, LLVM
  for BOLT).
- The qOS-kernel (§5.1) becomes a package in the frozen repo, config diffs
  reviewed like source.

Nothing in this plan requires "rebuild against whatever compiler ships today."

## 4. §1 — Compile-time optimization (Phase 1, low risk, immediate)

Workspace-root `Cargo.toml`:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
overflow-checks = false
debug = "line-tables-only"   # cheap; needed for BOLT + perf flamegraphs
incremental = false           # incremental fights LTO caching
```

`.cargo/config.toml`:

```toml
[build]
rustflags = [
  "-C", "target-cpu=x86-64-v3",
  "-C", "force-frame-pointers=yes",
]
```

**Target CPU floor is `x86-64-v3`** — Haswell/Excavator (2013+), ~99% of installed
hardware. Not `native` (breaks reproducibility) and not `x86-64-v4` (too narrow —
only very new server + top-tier laptop chips have AVX-512).

For the shell and hot apps, add `panic_immediate_abort` on release — kills the
panic-string bloat which is meaningful for a compositor + 45 apps.

**Expected wins**: 10–25% smaller binaries; 5–15% throughput on
encode/decode-heavy paths (openh264, HEVC, Automerge apply); compile time roughly
2× — mitigated by sccache-cluster on CI and by keeping `[profile.dev]` at cargo
defaults so local iteration stays fast.

## 5. §2 — Kernel & boot structural cuts (Phase 2)

### 5.1 qOS-kernel — no initramfs, built-in essentials

Build a `qos-kernel` package in the frozen repo from the Fedora kernel source with
a trimmed config:

**Statically compiled in:**
- virtio-{blk, scsi, net, gpu, rng} (KVM path)
- btrfs, xfs, ext4 (rootfs candidates)
- NVMe + AHCI + xHCI/eHCI (bare-metal storage + input)
- Common laptop NICs (Intel/Realtek families)

**Config changes**:
- `CONFIG_PREEMPT=y`
- `CONFIG_HZ=1000`
- `CONFIG_TRANSPARENT_HUGEPAGE_ALWAYS=y`
- `CONFIG_SCHED_MC=y`
- `CONFIG_DEBUG_*=n` (where safe)

Everything else stays as loadable modules. Removes the initramfs unpack → mount →
switch_root sequence entirely.

**Expected**: 300–800 ms off cold boot.

### 5.2 systemd-boot over GRUB

bootc supports systemd-boot via bootloader spec in `os/Containerfile`.
**Expected**: 100–300 ms + a simpler boot chain.

### 5.3 Service prune + socket activation

Audit `systemctl list-unit-files --state=enabled`; mask everything not on the
critical path. Move late services (bootc-fetch-apply-updates, chronyd initial
sync, most man-db timers) to socket-activated / timer-driven.

Concrete initial candidates to mask on the desktop image:
`NetworkManager-wait-online.service`, `systemd-networkd-wait-online.service`,
`plymouth-quit-wait.service`, `man-db-cache-update.timer`.

**Expected**: 200–500 ms + a cleaner `systemd-analyze critical-chain`.

## 6. §3 — Runtime dispatch & perceived responsiveness (Phase 3)

### 6.1 CPU governor policy

`qos-power.service` (small Rust helper + systemd unit) flips CPU governor based on
AC power state:

- AC connected: `performance` on `cpufreq_driver=amd-pstate` /
  `intel_pstate=active`
- Battery: `schedutil` or `powersave` (per-manufacturer default)

Uses `/sys/class/power_supply/AC/online` + udev events, not polling.

### 6.2 THP always

Complements the qOS-kernel THP config with a boot-time write to
`/sys/kernel/mm/transparent_hugepage/enabled`.

### 6.3 Vello + wgpu shader precompile

Compile Vello's shaders at compositor build-time to `SPIR-V`/`DXIL`/`MSL`. Ship
in `/usr/share/qos/shaders/` and load at compositor start, keyed by wgpu adapter
identity. Fall back to JIT compile only for unknown adapters.

**Expected**: 100–200 ms off shell startup.

### 6.4 cosmic-text glyph atlas warmth

Precompute a canonical atlas at OS-image build time — basic Latin, common CJK, a
core emoji subset. Ship in `/usr/share/qos/glyph-atlas.bin`, `mmap` at compositor
start. Cold-start latency of first line of text drops from "you can see it happen"
to "invisible."

## 7. §4 — Multi-ISA qOS app distribution (Phase 5, `qos-install` extension)

This is the one qOS-application-layer change. Existing `qos-install` /
`.qpkg` machinery already knows how to install content-addressed packages; we
extend it with an ISA-level dimension.

**Package format**: manifest declares which ISA levels this build supports; the
content-addressed payload is per-level. `notes` may ship as
`notes.v2.qpkg` + `notes.v3.qpkg` — the registry advertises both.

**Install-time selection**: `qos-install` reads `getauxval(AT_HWCAP2)` +
`/proc/cpuinfo`, picks the highest ISA level supported by both the target CPU and
the package manifest.

**Repair path** (self-healing without a bricked install):

1. Sandbox shim (`__sandbox-exec`) installs a SIGILL signal handler.
2. On a SIGILL from a newly-installed binary, the shim records the offending
   binary hash + ISA level in `/var/lib/qos-install/isa-blacklist.json`.
3. `qos-install run <app>` checks the blacklist first; if the current-installed
   variant is blacklisted, it silently re-fetches the next-lower ISA-level
   variant + swaps.
4. Emits a user-visible notification via the compositor's toast layer so users
   see "notes downgraded to x86-64-v2 — your CPU can't run v3 code" rather than
   a mysterious slowdown.

**Publisher pipeline**: emits v2 + v3 by default; v4 is opt-in per app manifest
(hot compute apps only).

**glibc-hwcaps path for shared libs we control** (cosmic-text glyph cache; any
future dynamic-linked qOS lib): populate
`/usr/lib/glibc-hwcaps/x86-64-{v2,v3,v4}/`. ld.so picks at load — zero
illegal-instruction risk, self-heals for free. We do NOT ship a hwcaps'd glibc
itself; Fedora's ships one and we consume it.

## 8. §5 — PGO on the hot binaries (Phase 4)

Instrument-build `shell` + `terminal` + `file-explorer` + `qos-install` with
`-C profile-generate`. Run a canonical scenario using the existing `shots
--mode recording` harness with instrumentation on — headless, deterministic,
already E2E-proven.

Feed the resulting `.profdata` into a second-phase build with
`-C profile-use=<path>`. **Commit the profile bytes into `os/pgo/`** — that's
what makes it reproducible under the frozen repo.

**Expected**: 5–20% throughput on the shell tick loop + widget draw path. Cost:
adds a "gather profile → commit" step to each release of a PGO'd binary.

## 9. §6 — BOLT post-link (Phase 6)

Run `llvm-bolt` on the PGO'd binaries. Reorders code by hotness, eliminates
cold-path bloat, improves iTLB + icache locality. Deterministic given the same
profile.

**Expected**: additional 5–15% on top of PGO.

Add to release pipeline as a final post-link step.

## 10. Sequencing

| Phase | Work | Independence | Dependencies |
|---|---|---|---|
| 1 | Compiler flags + x86-64-v3 floor | Cargo config only | none — start immediately |
| 2 | qOS-kernel + systemd-boot + service prune | `os/` only | frozen-repo snapshot bump |
| 3 | Governor + shader precompile + glyph atlas | shell + os | Phase 1 lands |
| 4 | PGO on hot binaries | pipeline change | Phase 1 stable |
| 5 | Multi-ISA `qos-install` | `qos-install` | Phase 1's v3 floor measured |
| 6 | BOLT | pipeline final step | Phase 4 lands |

Phases 1 + 2 can proceed in parallel; the rest sequentially.

## 11. Metrics

The performance work is measured, not asserted. Every phase reports against these
metrics, captured on a canonical target (Framework 13 + a generic KVM guest):

- **Cold boot** — `systemd-analyze critical-chain` head-to-`multi-user.target`
- **Shell first-paint** — `shell::App::new` start to first Vello submit
- **Shell tick throughput** — `perf record` flamegraph on a
  boot→login→terminal→drag scenario
- **App cold-launch** — `qos-install run <app>` to first paint, per app
- **Motion-to-photon on shared-flatpak-viewer** — already instrumented via the
  clock-offset handshake in `SurfaceStreamStart`
- **Binary sizes** — `size` on shell + apps, tracked per phase
- **Release build wall-clock** — CI dashboards it, dev worktrees log it

Baselines get captured on `main` at spec-approval time; phase-N wins report as a
diff against baseline plus phase-N-minus-1.

## 12. Risks

**PGO profile drift**: a stale profile makes the PGO'd binary *slower* than plain
LTO. Regenerate every release; treat stale `os/pgo/*.profdata` as a release
blocker.

**qos-kernel drift from upstream security patches**: our kernel package must
track Fedora security updates. Automate the config diff in the snapshot-rpms
workflow so a Fedora kernel bump either lands cleanly or fails visibly.

**Multi-ISA repair-path bugs**: a silent-downgrade footgun risks users running
v2 on v3-capable hardware indefinitely. Mitigation: emit persistent user-visible
notifications, keep a `qos-install isa-report` command that lists installed
downgrades so users can retry after a system update.

**BOLT × Rust interaction**: BOLT is proven on C/C++ but hasn't been widely tested
on our exact Rust + LLVM combination. Roll out on shell first; gate expansion to
other binaries on measured wins.

**Compile-time explosion**: LTO + PGO + BOLT together make a workspace release
build 3–4× slower. sccache-cluster on CI + `[profile.dev]` at defaults for local
iteration keeps this bounded.

## 13. Grounding index

- `os/Containerfile` — the bootc image build; systemd-boot + qos-kernel land
  here.
- `os/dev/snapshot-rpms.sh` + `.forgejo/workflows/snapshot-rpms.yml` — the frozen
  RPM repo pipeline (extend for qos-kernel).
- `crates/packaging/install/` — `qos-install`; multi-ISA extension lives here.
- `crates/packaging/pkg/` — `qos-pkg`; manifest schema gets the ISA-level field.
- `ui/src/text/` + `ui/src/renderer/` — cosmic-text + Vello integration; glyph
  atlas + shader precompile hooks.
- `shell/src/main.rs` — compositor entry point; shader-cache load fires here.
- `crates/session/qos-power/` (new) — governor policy service.
