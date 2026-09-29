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

## Actual hardware and CI recommendation

AWS inventory on 2026-09-29 showed two running `r7a.metal-48xl` CI machines
(`ci-x86-2` and `ci-x86-3`) in `us-west-2`. They are Talos Kubernetes workers
in the `ci-x86` pool, managed by OpenTofu; they are not general SSH hosts for
installing a Nix daemon. The EKS API currently admits only three configured
public CIDRs, none of which is this development machine. AWS login alone does
not supply cluster access. No production infrastructure was changed.

Given that topology, use Argo Workflows as the candidate CI execution plane.
Argo CD deploys its controller/templates, Forgejo sends commit or merge
candidate events to a thin status/admission bridge, and Argo creates jobs with
explicit CPU, memory, scratch and node requirements on the existing x86/ARM
pools. This replaces the custom Forgejo Actions runner broker rather than
adding a parallel runner fleet. Keep the existing affected-package planner and
build scripts at first; turn their selected work into a generated DAG instead
of embedding product decisions in workflow YAML. Preserve the current merge
queue's candidate SHA and status contract until the new path proves equivalent.

Nix and BuildStream remain candidates for the artifact/build graph, not the
cluster scheduler. Nix can run in a container or persistent builder pod, but
its store, binary cache, privileged daemon and KVM access need deliberate
design; the current monolithic source derivation already invalidates on a
README edit. Hydra would add a separate NixOS/PostgreSQL control plane and
compete with Kubernetes for scheduling. It is no longer the default choice
for this Talos/Argo CD environment. If Nix's artifact graph wins a measured
fit test, invoke it from the same Argo workflow and give it a dedicated cache
and builder pool. For PR-controlled code, isolate builders from signing keys.

## VM development plane

`tilo-vm` already has libvirt/QEMU adapters, copy-on-write disks, guest RPC
through `tilo-vmd` over SSH, semantic `tilo-control` actions, slot screenshots,
hypervisor screenshots, and serial consoles. Extend that protocol and CLI to
address a remote host; do not introduce a CI-specific action API. The portable
part is a Tilo machine endpoint (actions, state, input, screenshots, logs).
The provider-specific part is VM lifecycle (allocate, snapshot, reset, destroy,
serial before the guest is ready). A physical Tilo machine implements the first
part and has no VM lifecycle.

For the current Talos hosts, test KubeVirt as the VM provisioner: VM lifecycle,
placement and volumes remain Kubernetes resources, while `tilo-vm` speaks its
existing action protocol to the Tilo guest. KubeVirt offers serial and
graphical consoles and snapshot APIs; verify storage-class snapshot support,
KVM device access and bootc disk boot in this cluster before adoption. A
small Tilo machine service can add leases, agent-scoped credentials and one
endpoint for actions/screenshots/serial. This service is independent of the CI
controller; CI and interactive agents are peer clients. A physical Tilo
machine implements the same machine-control half without a VM lifecycle.

## Decision test on the actual metal host

Run the same three worktrees through the existing path and a candidate Nix
graph: (1) shell-only edit, (2) shared toolkit edit, (3) boot-chain edit.
For each, record changed derivations, executed CPU-minutes, cache bytes moved,
time to usable Tilo VM, KVM result, and human interventions. The candidate
must make the existing bootc image and its VM tests first-class outputs; a
successful Nix build of `tilo-session` alone is insufficient. Run one
representative PR burst through Argo Workflows on the existing pool. Compare
native scripts, Nix and BuildStream on the three changes before selecting the
build graph. Migrate one output class at a time. Keep the present Forgejo
merge gate active until the replacement emits equivalent status for the
actual merge candidate.

BuildStream remains a valid competing graph engine for the bootc image. A
linear build graph, VM provisioning layer, and CI control plane may be
separate as long as their ownership and artifact contracts are explicit.
