# qOS hardening roadmap (OS layer + compositor)

Status: APPROVED 2026-06-29. Decisions locked: **multi-stage container build**
(no RPMs yet); **unify the compositor render stack on wgpu/vello** (off iced).

Execution is one-at-a-time, top-to-bottom, but tracks are independent so the
compositor (`shell`) and packaging/infra (`os/`) run in parallel.

## Track A — Packaging (Pillar 1)
- **A1. Multi-stage `Containerfile`** — a builder stage compiles the Rust
  workspace in-image; the final stage copies the binaries from it. Removes the
  host/distrobox + F43→F44 glibc coupling; reproducible, self-contained build.
- **A2. CI → registry** — build the image in CI, push to a registry.
- **A3. Update path** — real `--target-imgref` + enable
  `bootc-fetch-apply-updates.timer`.

## Track C — Broken core + independent `os/` infra
- **C1. Fix `fs.service` crash-loop** (P2P FUSE — core feature, currently dead).
- **C2.** time sync (chrony) · **C3.** zram swap · **C4.** system locale ·
  **C5.** firewall (firewalld + peer-sync ports) · **C6.** audio session
  (pipewire/wireplumber enablement) · **C7.** bluetooth (bluez) ·
  **C8.** power-profiles-daemon + suspend/lid hooks.

## Track B — Compositor (Pillar 2), on wgpu/vello
- **B1. GPU-composited DRM scanout** — zero-copy GBM/EGL (or DMA-BUF) +
  hardware cursor, replacing CPU dumb-buffer readback. The foundation.
- **B2. Proper DRM input** — xkb keyboard, then gestures/IME (drop the `cage`
  crutch for the session).
- **B3. Multi-output** — multi-connector + hotplug + config.

## Track D — Daily-driver UX (depends on B1)
- **D1.** notifications · **D2.** session-lock + idle/DPMS · **D3.** AT-SPI.

## Track E — Security & install hardening
- **E1.** SELinux policy for qOS components (drop installer `enforcing=0`).
- **E2.** Secure Boot validation + disk encryption (LUKS/TPM in installer).

## Linear order
A1 → A2 → A3 → C1 → B1 → (C2–C8 interleaved) → B2 → B3 → D1 → D2 → D3 → E1 → E2.

## Reality flags
- A2 needs a CI/registry target (TBD with user).
- B-track edits the shared `shell` submodule — coordinate with the concurrent
  crdt-file-formats session.
- C8 suspend, B3 multi-monitor, E2 Secure Boot are only *fully* verifiable on
  real hardware; QEMU gets partial coverage.
