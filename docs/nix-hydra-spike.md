# Nix/Hydra fit test for Tilo

Status: evaluation, not a CI migration decision. Baseline: `13b80ef232` on
2026-09-29. The existing `flake.nix` exports a NixOS `tilo-session` package and
module for x86-64 and ARM64; it does not build the bootc OS image, kernel,
installer, or VM test fleet.

## Executed probe

Run in a Fedora ARM64 WSL host using the official `nixos/nix:2.35.2` container.
This environment has no `/dev/kvm` and 15 GiB of RAM. The source was copied
outside Git into a read-only container mount so test edits could not modify the
checkout.

- `nix flake check --no-build --all-systems` initially failed: Cargo's pinned
  Git dependencies had no `cargoLock.outputHashes` in `dist/nix/package.nix`.
  The hashes were fetched and added; both architectures then evaluated.
- The `cargoDeps` derivation actually built. A warm repeat took 2.57 seconds;
  that is a cache lookup, not a Rust compilation timing.
- An unrelated README edit changed the `tilo-session` derivation path from
  `q663v3lrhbyib8lxbnpanwzw9m0bz8k1` to
  `cgmk9djfc5dnpni2pyw6khmrfsav1781`, while the `cargoDeps` output stayed
  the same. The current source filter includes the whole monorepo, so this
  package does not yet give affected-component reuse.
- A cold ARM64 `nix build --dry-run` of `tilo-session` reported 1,246
  derivations. No full compositor build was run; this constrained ARM64 WSL
  environment would not measure the production x86 metal path.
- The Hydra package itself was fetched and executed. `hydra-eval-jobset`
  immediately required PostgreSQL; no Hydra jobset was run. Hydra upstream
  documents NixOS as its supported service host.

These facts establish that the current Nix packaging can be repaired and
evaluated. They do not establish that Hydra is a good Tilo CI control plane,
that Nix can build the bootc OS efficiently, or that Nix's artifact boundaries
match Tilo's development loop.

## Hardware topology to test

The AWS 192-core/768-GiB metal host should initially remain a Linux/KVM host.
Install the Nix daemon beside the current infrastructure; put its store and
build temporary directory on local NVMe. Run Hydra and PostgreSQL on a small
separate NixOS machine. Register the metal host as an `ssh-ng` Nix builder with
`x86_64-linux` and measured `maxJobs`, `speedFactor`, and supported features.
Only advertise `kvm` on a builder that can actually run the selected VM tests.
Nix's `maxJobs` counts concurrent derivations, not CPU cores; a 192-core host
must not be configured as 192 unconstrained Rust/image builds. Set job count
from measured per-job memory and CPU usage, with explicit capacity reserved
for interactive development VMs. Add ARM64 as a separate native builder.

The Nix store/cache and the VM image/artifact store are different products.
Keep both on fast local storage and push accepted immutable results to a
remote cache or object store. The Hydra evaluator must not be the large build
machine. For PR-controlled code, use an isolated builder trust domain rather
than treating Nix's build sandbox as a complete adversarial boundary.

## VM development plane

`tilo-vm` already has libvirt/QEMU adapters, copy-on-write disks, guest RPC
through `tilo-vmd` over SSH, semantic `tilo-control` actions, slot screenshots,
hypervisor screenshots, and serial consoles. Extend that protocol and CLI to
address a remote host; do not introduce a CI-specific action API. The portable
part is a Tilo machine endpoint (actions, state, input, screenshots, logs).
The provider-specific part is VM lifecycle (allocate, snapshot, reset, destroy,
serial before the guest is ready). A physical Tilo machine implements the first
part and has no VM lifecycle.

Use libvirt on the metal host for provisioning first. It already exposes a
remote management URI and supports serial and screenshots. `tilo-vm` currently
allows a libvirt URI in `vms.toml`, but its disk creation/XML writes and
`127.0.0.1` guest SSH forwarding still assume that `tilo-vm` runs on the VM
host. The first remote implementation can therefore run a `tilo-vm` host agent
over SSH on that machine, while the client forwards commands and streams
artifacts. Add leases, quotas and per-agent disk overlays only when multiple
agents use it. This VM service is independent of Hydra; CI may request a VM
from it, but interactive agent sessions also use it directly.

## Decision test on the actual metal host

Run the same three worktrees through the existing path and a candidate Nix
graph: (1) shell-only edit, (2) shared toolkit edit, (3) boot-chain edit.
For each, record changed derivations, executed CPU-minutes, cache bytes moved,
time to usable Tilo VM, KVM result, and human interventions. The candidate
must make the existing bootc image and its VM tests first-class outputs; a
successful Nix build of `tilo-session` alone is insufficient. Stand up a
Hydra jobset against those outputs on the separate NixOS controller and run
one representative PR burst. If it removes the current broker incidents while
preserving selective reuse and the `tilo-vm` development loop, migrate one
output class at a time. Keep the present Forgejo merge gate active until the
replacement emits equivalent status for the actual merge candidate.

BuildStream remains a valid competing graph engine for the bootc image. A
linear build graph, VM provisioning layer, and CI control plane may be
separate as long as their ownership and artifact contracts are explicit.
