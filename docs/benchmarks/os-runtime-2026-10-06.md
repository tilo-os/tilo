# Full-OS runtime pilot, 2026-10-06

This is a fixture qualification and a single emulated development-image pilot,
not a current-main performance baseline or evidence of an OS speedup. Raw
observations, artifact provenance and excluded measurements are in
[os-runtime-2026-10-06.json](os-runtime-2026-10-06.json).

## Fixture

The public `tilo-vm-disk/main-d8718c2-ext4-ci` x86-64 disk was downloaded on
the remote build runner and verified against its published SHA-256:
`a558d13585d41674dc7f08fca384be6683826fbab770d2071b29311f8a6e85aa`.
Its source is `d8718c2ab06ec0fc60ef81708d4bfc52816f631c`, not the task's
current-main base `60f484b76cbde5304535beaac369db7ad5084434`. The development
image includes compilers and compatibility test applications.

An independently writable qcow2 child booted through `os/dev/vm.sh`, with a
private runtime directory, cycle and loopback SSH port. Settings were 4 vCPUs,
4096 MiB, explicit multithreaded TCG, virtio 2D at 1280×720 and the harness's
default firmware/security settings. No physical desktop was launched, no
guest credential or persistent access grant was added, and the base disk
remained read-only. The guest was stopped after measurements.

The runner has no `/dev/kvm` and no `/dev/dri/renderD*` node. Its `card0`
does not supply the render node required by the harness's host-GL path.
Do not compare these TCG/software results against KVM or physical machines.

## Observations

| Observation | Result | Scope |
|---|---:|---|
| Kernel + initrd + userspace | 54.273 s | Guest `systemd-analyze time`; firmware excluded |
| Userspace | 32.468 s | Same boot |
| Graphical target | 30.534 s into userspace | Unit activation, not interactive readiness |
| Linker-cache rebuild | 10.766 s | Critical-chain `ldconfig.service` |
| Desktop idle CPU | 0.872% mean; 0.399–1.844% | Six five-second intervals; aggregate guest CPU capacity |
| RAM unavailable | 880.1–892.6 MiB | `MemTotal - MemAvailable`, including observer/kernel |
| Shell RSS | 365.3 MiB | Single endpoint sample; shared pages included |
| Development `/sysroot` usage | 14,650,843,136 bytes | Guest `df`, includes development image/storage |
| Downloaded qcow2 file | 6,045,630,464 bytes | Artifact length, not installed size |

The real greeter reported that it accepted input. The selected-account form
required `vm.sh login --passcode-only`; the username-and-password sequence
returned to sign-in with an incorrect-passcode status. The passcode-only
journey started the actual `shell`, filesystem and session services. No native
application was launched, and first-use provisioning was not completed.

The idle collector used one SSH connection and Python stdlib to read
`/proc/stat` and `/proc/meminfo` seven times, sleeping five seconds between
reads. It performed no other guest work during that window. CPU busy is
`100 * (total - idle - iowait) / total` from successive tick differences;
`total` sums the first eight counters to avoid counting guest time twice.
Values describe the guest, not the host QEMU process. The SSH/Python observer
is included. RSS is not additive across processes and is not PSS.

An earlier greeter-idle attempt overlapped sign-in and is excluded. The pilot
host-to-ready interval also includes an untimed gap between boot and starting
the readiness watcher, so it is excluded. Neither is a startup or idle result.

The unprivileged `/usr` scan reported 12,698,312,704 bytes but failed on
unreadable directories; it is preserved as partial data only. `bootc status`
requires root and failed. Neither establishes complete installed production
image size. No permission changes were made to obtain either measurement.

## Fixture improvement and next valid comparison

The raw-QEMU `tilo-vm` adapter previously always requested host GL and passed
only the SSH port, ignoring its registered CPU count, memory, architecture,
resolution and emulation setting. Consequently a registry entry was not an
accurate description of the machine measured. It now passes these settings to
the existing harness, uses 2D by default, and supports `--gl` and `--resolution`
at creation. This fixes reproducibility and allows the existing software boot
path on runners without a render node; it is not an OS performance optimization.

Current main already builds the linker cache and installs its conditional
boot service drop-in. The old disk's linker-cache cost therefore supports
testing that existing change, not duplicating it or claiming another gain.

To establish performance targets, use a current, immutable production image
and a KVM-capable runner with a known renderer. Preserve its disk/image/source
hashes, actual QEMU arguments, guest boot ID and observer settings. Start the
host readiness timer immediately before the existing harness boot command and
require the greeter's `/accepts_input` result. Repeat on fresh children of the
same base, stating host cache conditions. Record greeter and signed-in idle
windows separately after warmup. Keep production installed usage separate from
the developer image, artifact length and virtual disk capacity.

Native app startup, input-to-present latency and active frame cadence remain
unmeasured in this pilot. Use the existing semantic control surface and
presentation-backed collectors in `scripts/perf/`, with an authorized session
that exposes them. Do not provision new access or substitute synthetic frame
counts for presentation evidence. The historical SHM conversion comparison in
[shm-conversion-benchmark.md](../shm-conversion-benchmark.md) remains a separate
matched CPU microbenchmark, not a whole-desktop or current-redesign result.
