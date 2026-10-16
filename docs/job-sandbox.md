# Job sandbox

`tilo-sandbox`'s `job` module (`crates/platform/tilo-sandbox/src/job`) runs one command as an isolated
job on a shared kernel. t, the build and test runner, is its first user: every
check t runs on a worker is one job. `ush`'s approved checks
([approved-check-runtime.md](approved-check-runtime.md)) are the second.

The decision is a shared kernel with namespaces, cgroups, seccomp and Landlock.
There is no VM per job. This document gives the reasons, the threat model, every
layer with its exact settings and measured cost, and how the callers use it.

## What a job needs

| Requirement | Target | Measured |
|---|---|---|
| Warm build directories | the workspace's `target/` stays mounted between jobs | kept: a bind mount, not a copy |
| Startup | < 100 ms | ≈ 3 ms (2.5 ms bare, 2.85 ms with seccomp and Landlock) |
| Build overhead | ≤ 1–5 % | ≈ 1–2 % |
| Neighbours | one job can't starve or kill another job or the worker | memory, PIDs, CPU, disk I/O and disk space bounded per job |
| Escape | only through a kernel bug | see [What an escape takes](#what-an-escape-takes) |

A VM per job fails the first two rows. Booting one costs a second or more.
Sharing a warm `target/` with a VM means virtiofs or a block device per
workspace, and incremental `cargo` builds hit the filesystem hard. bubblewrap
and nsjail meet the targets but start slower (3.85 ms and 5.2 ms on the same
nodes). Neither can place the job into its cgroup atomically, so a fork bomb
started in the first instructions runs before the limits apply.

## Threat model

Most of the code that runs is mostly trusted: commits on `main` and `stable`,
written by the team and its agents. The attacks that actually happen are, in
order:

1. **Noisy neighbours.** A test that leaks memory, a build that forks without
   bound, a script that fills the disk, a daemon a test starts and never stops.
   These happen weekly, and on a shared worker they take other jobs and the
   worker down with them. Most of this design is about them.
2. **A compromised dependency.** A build script, proc macro or test of a crate
   in `Cargo.lock` runs with the job's authority. It must not get more than the
   job had: no other job's files, no worker credentials, no route to the
   cluster.
3. **A kernel exploit.** The kernel is shared, so a kernel bug is the one way
   out we accept. We still shrink the attack surface: the syscalls behind
   recent user-namespace exploits are refused.

**Goal: getting out of a job takes a kernel escape, not a misconfiguration.**
Escaping must give an attacker nothing worth more than the job already had.
Secrets never rest on job nodes, as described under
[Secrets and publishing](#secrets-and-publishing).

Out of scope: side channels between jobs on one node (cache timing, Rowhammer),
and denial of service through kernel locks that no cgroup accounts.

## The process tree

```text
worker (t / ush)                       caller's namespaces, its own cgroup leaf
 └─ job init (pid 1)                   clone3 → new user, mount, pid, net, ipc, uts, cgroup ns,
     │                                 born inside the job's cgroup (CLONE_INTO_CGROUP)
     └─ the command                    fork + execve; everything it starts stays in the job
```

The library calls `clone3` from the worker with `CLONE_INTO_CGROUP`,
`CLONE_PIDFD` and `CLONE_NEW{USER,NS,PID,NET,IPC,UTS,CGROUP}`. The child is born
inside the job's cgroup, so no instruction of the job ever runs outside its
limits. The worker keeps the pidfd and waits on it without touching `SIGCHLD`.

The child runs no allocator and takes no lock, because it is a copy of a
multithreaded process. Every path, argument, mount, Landlock rule and the seccomp
program is prepared before `clone3`. After it the child makes raw system calls
only. In order, the child:

1. Waits on a pipe until the worker has written its `uid_map`/`gid_map` and run
   the caller's setup hook. t's hook attaches pasta to the job's network
   namespace and installs its nftables. If the worker closes the pipe instead,
   the child exits.
2. Makes `/` private (`MS_REC|MS_PRIVATE`), then builds the new root on a
   tmpfs. The root is either an image's unpacked root filesystem, bound
   read-only, or the host's `/usr` and `/etc` bound read-only with
   `/bin`→`usr/bin` style links.
3. Mounts what the job asked for: workspace and target read-write, a private
   `/tmp`, a fresh `/proc` for the job's PID namespace, and a minimal `/dev`
   (`null zero full random urandom tty`, `devpts` with a new instance, `shm`)
   plus devices it declared. It mounts a tmpfs `/sys` that holds only the job's
   own `cgroup2` (read-only), so `nproc` and `available_parallelism` see the
   job's CPU limit.
4. `pivot_root`s into it and detaches the old root.
5. Sets the hostname, and brings up `lo` in the private network namespace.
6. Drops every capability: bounding set, ambient, effective, permitted and
   inheritable. Sets `SECBIT_NOROOT` (locked), so an `execve` as uid 0 inside
   regains none.
7. Sets `no_new_privs`, then Landlock, then seccomp.
8. Marks itself non-dumpable and zeroes the environment it inherited from the
   worker. Even a process with the same uid inside the job can then neither
   read `/proc/1/environ` nor attach to pid 1.
9. Closes every descriptor except stdio, a status pipe and those the caller
   passes explicitly (`close_range`). Without that, a job would hold another
   job's pipe open, or the worker's server connection.
10. Forks the command, which calls `setsid`, `dup2`s its stdio, `chdir`s and
    `execve`s with exactly the environment it was given. The `PATH` search runs
    inside the sandbox, on a stack buffer.
11. Reaps every process reparented to it. Zombies would otherwise hold slots of
    `pids.max`. When the command exits, pid 1 writes its wait status to the
    status pipe and exits. The kernel then kills everything left in the PID
    namespace.

A setup step that fails writes its step number and `errno` to a report pipe. The
caller gets "mounting /proc: Operation not permitted", not an exit status 127.

Startup cost is dominated by `clone3` of the worker's page tables and the
mounts. The benchmark (`cargo test -p tilo-sandbox --test job startup::`)
reports the median of 200 spawns of `/bin/true`. It fails above 100 ms.

## Layers and settings

### Namespaces

| Namespace | Why |
|---|---|
| user | the job is uid 0 *inside*, mapped to one uid outside (a single-uid map, `setgroups` denied), or to a block of host ids of its own (below); everything else on the host is `nobody` |
| mount | its own root; nothing it mounts or unmounts reaches the worker |
| pid | sees only its own processes; `/proc/1` is the job's init, not the worker |
| net | its own `lo`; servers it starts on `127.0.0.1` are invisible to other jobs and to the node; no route out unless egress is attached |
| ipc, uts | SysV IPC and hostname of its own |
| cgroup | `/proc/self/cgroup` is `/`; its `/sys/fs/cgroup` is its own subtree |

The outside uid is the worker's own by default. When the worker runs with
`hostUsers: false` (the target), that uid is already unprivileged on the host. A
privileged worker can choose an unprivileged `job_uid` instead. The job's
writable directories must then be owned by it.

**Host users of a job's own.** The kernel keeps some limits per host user, not
per namespace: pipe buffers (`fs.pipe-user-pages-soft`: past 1024 full pipes a
user's new pipes get two pages, and programs that write 64 KiB before reading
deadlock), `RLIMIT_NPROC`, inotify instances and watches, keys, POSIX message
queues. Jobs that all run as the worker's uid share them. A root worker gives
each running job a block of host ids instead (`IdMap::block`, 65536 ids that no
other running job has): the job's namespace maps `0..65536` onto it, so the job
is a host user of its own. Its files stay the worker's: every host path bound in
is an idmapped mount (`mount_setattr` with `MOUNT_ATTR_IDMAP`, Linux 5.12) on
which the worker's files show as the job's and what the job creates belongs to
the worker on the host, so the next job, with another block, finds a warm build
directory as its own. The worker makes these mounts before the job starts
(idmapping takes `CAP_SYS_ADMIN` over the filesystem) and the job's init
attaches them (`move_mount`); overlays get idmapped layers (Linux 5.19). The
init takes on its ids in the namespace and drops the worker's supplementary
groups. A writable path whose filesystem can't be idmapped fails the job; a
read-only one, or a device on devtmpfs, is bound as it is. t hands the blocks out
from `0x70000000` up and falls back to the shared uid, loudly, where a trial job
shows it can't.

### Filesystem

| Path | Mount | Landlock |
|---|---|---|
| `/` | image root or tmpfs with `/usr`, `/etc` read-only | read, execute |
| workspace (`/src` in t) | bind, read-only or read-write per check | full rights when read-write |
| target (`/target` in t) | bind, read-write: the warm build directory | full rights |
| `/tmp` | bind of the job's scratch directory (on disk: builds write gigabytes there) | full rights |
| `/dev` | tmpfs with bound device nodes | full rights (`IOCTL_DEV` included, for ttys) |
| `/proc` | new procfs of the job's PID namespace | read |
| caches | binds the caller names (cargo's registry, sccache) | full rights when read-write |

Landlock handles every filesystem right of the running kernel's ABI. A right the
ruleset handles and no rule grants is denied:

- **`REFER` is handled and granted** beneath every writable path. rustc renames
  across directories (incremental compilation's session directories). Without
  `REFER` those renames fail with `EXDEV`, and builds fail in ways that look
  like filesystem bugs.
- **No TCP rules** (`LANDLOCK_ACCESS_NET_*`). The bind rule breaks every test
  that binds a loopback port, and the private network namespace already makes
  binding harmless.

### Capabilities

All capabilities are dropped before `execve`, and `SECBIT_NOROOT` is locked. A
job has no `CAP_NET_ADMIN` over its own network namespace, so it can't reach
nf_tables through netlink. This closes the same exploit class as the seccomp
rules below, in a second layer. It also can't undo what the worker set up in
that namespace (pasta's routes, the egress filter).

### Seccomp

The CI profile is in `tilo-sandbox-core` beside the application baseline:
`sandbox_core::seccomp::ci_program(CiProfile)`. It is a deny-list that answers
`EPERM` and allows everything else. Builds run arbitrary toolchains, and an
allow-list breaks whenever glibc adds a syscall.

| Refused | Why |
|---|---|
| `bpf`, `perf_event_open`, `userfaultfd` | the verifier, perf and fault-timing are the usual first step of a kernel exploit |
| `keyctl`, `add_key`, `request_key` | kernel keyrings: a recurring source of use-after-free bugs, and no build needs them |
| `io_uring_setup`, `io_uring_enter`, `io_uring_register` | the largest recent source of kernel CVEs |
| `mount`, `umount2`, `pivot_root`, `fsopen`, `fsconfig`, `fsmount`, `fspick`, `move_mount`, `mount_setattr`, `open_tree` | the job's filesystem is final |
| `setns` | no joining another namespace |
| `unshare`, `clone` with any `CLONE_NEW*` flag | no new user namespace, and with it no fresh capabilities over a new network namespace (the nf_tables class, e.g. CVE-2024-1086) |
| `clone3` → `ENOSYS` | seccomp can't read `clone3`'s flags; glibc falls back to `clone`, whose flags it can |
| `kexec_load`, `kexec_file_load`, `init_module`, `finit_module`, `delete_module` | kernel replacement and modules |
| `quotactl`, `quotactl_fd` | the job's disk quota is set from outside |
| `personality` except the query (`0xffffffff`) and `PER_LINUX` | `ADDR_NO_RANDOMIZE` and `READ_IMPLIES_EXEC` weaken exploit mitigations |
| `open_by_handle_at`, `name_to_handle_at` | file handles bypass the mount tree (the "shocker" escape) |

`ptrace` stays allowed. Debuggers and crash handlers in tests use it, and it
reaches only the job's own processes, because pid 1 is non-dumpable and every
capability is gone.

**The nested profile** (`CiProfile::Nested`, selected with
`JobSpec::allow_nested_sandboxes`) is for tests that start sandboxes of their
own, such as bubblewrap or `unshare`. It allows new namespaces of every kind
and the mount calls. In a nested user namespace the job holds `CAP_NET_ADMIN`
over its own network namespace, so `socket(AF_NETLINK, …)` with
`NETLINK_NETFILTER` or `NETLINK_XFRM` stays refused, checked by argument.
`NETLINK_ROUTE` stays allowed, because bubblewrap brings up `lo` with it.
`setns`, `bpf`, io_uring, the keyrings, `userfaultfd`, `perf_event_open`,
kexec and modules stay refused too. Such a job runs without Landlock, which
refuses every mount to a process it confines. t selects the profile per check
with `nested_sandboxes`. Such a job is uid 1000 (`NESTING_ID`) inside, not
root: its root would hold no capability, and since Linux 5.12 mapping the
parent namespace's uid 0 into a new user namespace (`unshare
--map-root-user`, bubblewrap) takes `CAP_SETFCAP`, so writing `uid_map` fails
with `EPERM`. As any other id, bubblewrap and `unshare` create their
namespaces as for an unprivileged user.

**The privileged profile** (`CiProfile::Privileged`) keeps everything above
except the namespace and mount rules. Checks that genuinely need real root
(rootful podman, buildah's chroot isolation, loop devices, mounts) no longer
need it or a pool of their own: they run as [micro-VM steps](#micro-vm-steps),
root in a guest kernel, with QEMU itself an ordinary strict job.

### cgroup per job

```text
<the worker's cgroup>/            the pod's container cgroup (kubelet sets memory.oom.group=1 here)
  worker/                         the worker process; memory.min = reserve
  jobs/                           memory.max = pod limit − reserve; controllers enabled
    <job>/                        one per running job
```

| File | Value | Why |
|---|---|---|
| `memory.max` | the job's size | its own bound |
| `memory.oom.group` | `1` | an OOM ends the whole job, not one compiler in it (a build missing one rustc fails confusingly) |
| `memory.swap.max` | `0` | containment means the limit holds |
| `memory.high` | **not set** | with no swap, `high` below `max` throttles the job into a hang instead of failing it |
| `pids.max` | `4096` | fork bombs stop here; the job's init reaps orphans, so zombies don't count against it |
| `cpu.weight` | proportional to the job's share of the node | fair under contention, full speed when idle |
| `cpu.max` | optional | for jobs that must not exceed a quota even on an idle node |
| `io.max` | `MAJ:MIN rbps= wbps= riops= wiops=` of the **whole disk** | the kernel rejects a partition's numbers; the library resolves the disk. `io.weight` isn't available: Talos kernels lack iocost |
| `cgroup.kill` | `1`, always, at job end | reliably ends `setsid` daemons, double-forked servers and listeners a test left behind |

`jobs/memory.max` is the sum the jobs may use together: the pod limit minus a
reserve for the worker. kubelet sets `memory.oom.group` on the container's
cgroup. A pod-level OOM therefore kills the worker and every job at once, and
must never happen. Capping `jobs/` below the pod limit makes the kernel choose
an OOM victim among the jobs instead. `memory.min` on `worker/` keeps the worker
from being reclaimed into a stall.

A job's `memory.max` may be above what the scheduler reserved for it: t gives
each job twice its reservation, at least 2 GiB more, so a build that briefly
needs more than it usually does gets it from what other jobs leave unused,
instead of failing. The budget therefore bounds reservations, not limits. The
provider refuses to create a job whose reservation (`Limits::memory_reserved`,
else its `memory.max`) would take the sum of running jobs' reservations past
the budget, so a scheduler that overcommits learns it at once. Overruns are
bounded twice: `jobs/memory.max` keeps all jobs together below the pod's
limit, and the worker's guard stops the job furthest past its reservation when
the node runs short, before the kernel has to pick a victim.

At the end of a job the library reads `cpu.stat`, `memory.peak`, `io.stat` and
`memory.events`. `oom_group_kill` tells "killed for memory" from a crash. Then
it writes `cgroup.kill`, waits until `cgroup.events` says `populated 0` and
removes the directory.

### Disk

Per-workspace and per-job scratch space is bounded with an **XFS project quota**:
the directory gets a project id with `PROJINHERIT`, the project a hard block
limit. At the limit, writes fail with `EDQUOT`. Applying a quota to a fresh
directory takes about 2 ms. A warm `target/` that already holds files is tagged
once by walking it, and new files inherit the id after that.

The prod build volumes (XFS on ZFS zvols) are mounted `noquota` today. The
library detects whether project quotas are enforced (`Q_XGETQSTAT` on the
filesystem). Without them, `set_quota` returns `Unsupported` with the reason,
the caller logs it once, and jobs run without a disk bound. The worker's own
disk-usage sweep then remains the only bound. Setting a project id and a limit
needs `CAP_SYS_ADMIN` on the host, so it goes through the
[cgroup provider](#the-cgroup-provider-direct-or-broker) like cgroups do.

### Network

Every job has its own network namespace with only `lo`. It may stay that way
(`Network::Loopback`), or the caller attaches egress from outside before the job
starts:

- **pasta** (`crates/runtime/egress`) connects the namespace to the node's
  network as a user-mode TCP/IP stack. No address in the namespace maps to the
  node's loopback (`--no-map-gw`).
- **nftables**, installed by the worker into the job's namespace through a
  netlink socket opened there, decides what may leave. Either:
  - `Reach::Hosts`: the DNS-allowlisted mode apps and `ush` environments use
    (only answered addresses of granted names), or
  - `Reach::PublicInternet`, for CI. Every destination is allowed except
    loopback, private (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`), CGNAT
    (`100.64/10`), link-local (`169.254/16`, `fe80::/10`, which includes the
    metadata service at `169.254.169.254`), multicast, reserved ranges, and the
    cluster's pod and service ranges the caller names. DNS goes to the
    worker's stub inside the namespace, which forwards every name.

The grant grammar both modes share used to live in `collab-types`, which pulls
in Tilo's identity and wire types. It now has a crate of its own,
`tilo-net-grants`, that `collab-types` re-exports. t can depend on egress
without the collaboration stack.

### Micro-VM steps

The third tier, after the strict default and nested sandboxes: a command that
needs real root runs as root in a guest kernel of its own, and the virtual
machine is an ordinary job. `job::vm` builds it:

- **QEMU in the strict sandbox**: the host's `/usr`, `/dev/kvm` as its only
  device, the CI seccomp filter, Landlock, no network namespace of its own but
  an empty one, and the step's files at fixed paths under `/vm`. Nothing runs
  as root on the host; the job's cgroup bounds QEMU, guest memory included.
- **Machine**: `microvm` on x86-64 (no ACPI, PIT, PIC, RTC or serial port; a
  triple fault resets it, which ends QEMU), `virt` on arm64; virtio-mmio
  devices only. The guest gets the step's cores and memory, and its own
  `/dev/kvm` (nested virtualisation) only when the step asks for KVM.
- **Guest**: a kernel and a static busybox, pinned by digest (t builds them
  reproducibly as a check's outputs, from the Kata guest kernel's
  configuration with the RAID6 benchmark off, which alone halved boot). Per step, `prepare` writes an
  initramfs with busybox, `vm/init.sh` and a generated job file.
- **Disks, virtio-blk only**: the step's image as a read-only ext4 (made by
  `make_image`: `mkfs.ext4 -d` in a user namespace of its own that maps the
  caller to root, so the files are root's and unreadable ones still copy), copy-on-write onto a sparse scratch ext4
  that also holds `/tmp`, `/var/tmp` and `/var/lib/containers`; more read-only
  images mounted read-only or copy-on-write (sources, earlier steps' outputs);
  and a raw output disk on which the guest writes a status line and a tar of
  the files matching the step's output patterns. The host never mounts what a
  guest wrote; `finish` reads the tar as untrusted, keeping only regular files
  and directories below its destination. virtio-fs is avoided: it is slow and
  breaks SQLite's write-ahead log. IO is QEMU's thread pool, since io_uring is
  refused.
- **Output**: the command's stdout and stderr on a virtio serial port (not a
  terminal), the guest's console next to it, both QEMU's stdout.
- **Network**: passt outside the sandbox, as the caller, reached on a unix
  socket (`-netdev stream`): outbound connections and DNS, nothing forwarded
  in, no address leading to the node's loopback.

Measured on a laptop and a CI node: a step boots in about 0.25 s to its
command once the RAID6 benchmark is off; it holds 0.2 to 0.4 GB besides what
its command uses; a rootful `podman build` and `podman run` inside take 1 to
2.4 s.

## The cgroup provider: direct or broker

Creating cgroups, delegating them and setting quotas needs either a writable,
delegated cgroup subtree or privilege on the host. The library hides that
behind one trait:

```rust
pub trait CgroupProvider: Send + Sync {
    /// A cgroup for one job, limits applied, ready for CLONE_INTO_CGROUP.
    fn create(&self, job: &str, limits: &Limits) -> Result<JobCgroup, ProviderError>;
    /// An XFS project quota on a directory (passed as a descriptor).
    fn set_quota(&self, dir: BorrowedFd<'_>, project: u32, bytes: u64) -> Result<QuotaOutcome, ProviderError>;
}
```

A `JobCgroup` holds the cgroup's directory descriptor, which `clone3` takes. It
reads the job's counters, and `kill()`s and removes the cgroup when it drops.

**`Direct`**: the worker's cgroup is writable and delegated. That is the case
for today's privileged pods, for a systemd unit with `Delegate=yes`, and for
`ush` under the user manager. At startup it moves every process of the worker's
cgroup into `worker/`, enables `cpu memory io pids` in `subtree_control`, and
creates `jobs/`. Quotas use `FS_IOC_FSSETXATTR` and `quotactl_fd` directly when
the process has `CAP_SYS_ADMIN`.

**`Broker`**: the worker runs unprivileged (`hostUsers: false`), where cgroupfs
is mounted read-only and quota calls are refused. A small privileged DaemonSet,
`tilo-job-broker` (the same crate, `--features broker`), listens on a Unix
socket on each node. Every request arrives over `SOCK_SEQPACKET`, and the broker
identifies the caller by `SO_PEERCRED`:

| Request | Broker does | Returns |
|---|---|---|
| `init reserve=` | finds the caller's pod cgroup from `/proc/<pid>/cgroup`; moves the pod's processes into `worker/` (`memory.min`), enables controllers, creates `jobs/` with the budget, chowns the pod cgroup's `cgroup.procs` to the caller's host uid | the budget |
| `create <job> <limits>` | creates `jobs/<job>`, writes the limits, chowns `cgroup.kill` to the caller | the cgroup's directory fd (`SCM_RIGHTS`) |
| `remove <job>` | `cgroup.kill`, waits for empty, `rmdir` | — |
| `quota <project> <bytes>` + dir fd | tags the directory (recursively, once) and sets the limit on the fd's filesystem | `ok` or `unsupported <why>` |

`CLONE_INTO_CGROUP` checks write permission on `cgroup.procs` of the common
ancestor of the source and target cgroups, and that the target is inside the
caller's cgroup namespace. The chown in `init` grants exactly that for the
caller's own pod, and nothing outside it. The broker refuses a job name with a
`/`, a peer that isn't in a pod cgroup, and a `quota` on a descriptor that isn't
a directory. It never runs anything the caller names. Reservations a caller
sets are capped by its pod's budget, and every job sits under its pod's
`jobs/`, so a compromised worker gains no more than its pod already had.

## Node pools

| Pool | Runs | Seccomp | Notes |
|---|---|---|---|
| **workers** | untrusted and trusted checks of `main`, branches, working copies, micro-VM steps included | CI | many jobs per node, warm build directories |
| **release** | jobs of trusted runs of `stable` and tags, which may receive secrets and identity tokens | CI | same sandbox, nodes that take nothing else |

Release nodes use the same sandbox as everyone else. What separates them is
scheduling: they take only jobs of trusted runs on `stable` and release tags, so
no untrusted code ever shares their kernel, and an escape from a release job
finds only other release jobs. A VM per release job buys little on top of that.
It costs the warm build directory and a second or more per job.

### Worker pods

Today's workers are privileged pods without a cgroup namespace. They can write
the host's cgroup root and `/dev/zfs`, so anything that escapes into the worker
is root on the node. The job sandbox closes this from the job side: a job has no
capabilities, can't create namespaces and is born in its cgroup. The worker pod
stays the wider boundary until the following lands.

The target pod spec:

- `hostUsers: false`: the worker's root is an unprivileged uid on the host.
- `securityContext.seccompProfile: Unconfined` and `procMount: Unmasked`. The
  runtime's default profile refuses `clone3` with namespace flags, and a
  masked `/proc` makes the kernel refuse a fresh procfs in the job's PID
  namespace. The job's own filter replaces the runtime's.
- `Broker` as the cgroup provider, with the broker DaemonSet on every worker
  node.

The infrastructure changes this needs, none of which the library makes:

1. The `tilo-job-broker` DaemonSet: privileged, `hostPID: true`, host cgroupfs,
   its socket in a `hostPath` directory mounted into worker pods.
2. `prjquota` in the build volumes' mount options (the StorageClass's
   `mountOptions`), so XFS enforces project quotas.
3. No CPU limit on worker pods (requests only). A CFS quota on the pod throttles
   every job together. Per-job `cpu.weight` (and `cpu.max` where wanted) does
   the sharing.
4. kubelet `podPidsLimit` above the sum of the jobs' `pids.max` plus the
   worker's own, so the pod limit never fires before a job's.
5. `kernel.io_uring_disabled=2` on worker nodes, so io_uring is closed even to
   a process that slips past the filter.
6. Release and privileged pools as separate node groups (taints and labels),
   and the worker pods of each pool started with their labels.

## Secrets and publishing

- No secret rests on a job node. Stored secrets live in the server's encrypted
  configuration. The server hands a job's secrets to that job's lease only,
  while it runs; nothing is written to the node's disk.
- Publishing uses t's short-lived OpenID Connect tokens: a job of a trusted run
  asks the server for a token per audience, valid for minutes and for one
  repository, reference and commit. The registry and the cloud exchange it for
  narrowly scoped credentials. There is nothing long-lived on a node to steal.
- Release jobs run in this sandbox, on nodes that take only `stable` and tag
  jobs.

## How t uses it

t's worker builds a `JobSpec` from a check:

- **root**: the check's image (unpacked once per node, read-only)
- **mounts**: sources at `/src` (read-only, read-write or an overlay), the
  workspace's build directory at `/target`, shared caches, the jobserver FIFO
  at `/run/t-jobserver`, the job's scratch directory at `/tmp`
- **environment**: exactly what the project and check declare, plus `PATH`,
  `HOME=/tmp`, `CARGO_TARGET_DIR`, `CARGO_MAKEFLAGS`
- **network**: `Loopback`, or `PublicInternet` through pasta, with the cluster's
  ranges denied
- **limits**: from the check's size: `memory.max`, `cpu.weight`, `pids.max`,
  `io.max`
- **seccomp**: CI, or nested for checks that start sandboxes of their own
- **micro-VM steps** (`vm = true`, or `privileged = true`): the check's image
  as the guest's root disk, made once per image and node; its sources, and the
  outputs of the checks it needs, as a read-only disk per step, copy-on-write
  in the guest; its outputs back through the output disk

t selects the runner with `sandbox = "job"` in the worker's configuration, with
`"bwrap"` (the previous runner) as the fallback. A worker that can't create the
namespaces logs why at startup and uses bwrap, as do nodes that run checks
without an image.

## How ush uses it

`ush` runs approved checks today through `systemd-run --user` (for cgroup limits)
and then bubblewrap. With the library, the user manager's delegated subtree
becomes a `Direct` provider (`Delegate=yes` on ush's unit), and one `spawn`
replaces both steps. The check is born in its cgroup, gets the same seccomp
profile and Landlock rules, and keeps ush's own admission and report logic.

## macOS

Not in this version. `JobSpec` is portable data, and the Linux runner is one
backend behind `tilo_sandbox::job::Launch::spawn`. A Seatbelt backend would compile the same
spec to an SBPL profile with `crates/packaging/launcher-macos/src/sbpl.rs` and
start the command with `sandbox_init`. It has no cgroups, so its `Limits` would
be `setrlimit` only. On other platforms `spawn` returns `Unsupported` today.

## What an escape takes

| From | To get | Takes |
|---|---|---|
| a job | another job's files or processes | a kernel bug (namespaces, Landlock) |
| a job | the node's loopback or the cluster | a kernel bug: there is no route (own netns; pasta maps nothing; nftables denies the ranges) |
| a job | more memory, PIDs or CPU than its share | nothing: the kernel enforces it, and the worker survives an OOM |
| a job | the worker's environment or memory | a kernel bug: pid 1 is non-dumpable, its environment zeroed, the job holds no capability |
| a worker (today, privileged) | the node | nothing more: closing this is the `hostUsers: false` + broker step |
| a worker (target) | the node | a second kernel bug |
| a release job | secrets of other runs | nothing on the node: secrets are per lease and in memory; identity tokens expire in minutes |

## Tests

`crates/platform/tilo-sandbox/tests/job/` needs a Linux host where unprivileged
user namespaces (or root) are available. Elsewhere every test prints why it
skipped. Limits tests also need a delegated cgroup; quota tests need
`JOB_SANDBOX_TEST_XFS` pointing at a directory on an XFS filesystem mounted with
`prjquota`. Run them where the kernel can: through `t`, or in a throwaway pod.

- isolation: a job can't see a neighbour's processes or files, can't reach a
  server on the host's loopback, and can read `/proc/<pid>/environ` only of
  its own processes;
- limits: an OOM kills only the job and is reported as one; a fork bomb stops
  at `pids.max` and the worker carries on; `cgroup.kill` removes `setsid`
  daemons; quotas return `EDQUOT` where available;
- seccomp: every refused call returns `EPERM` (`clone3`: `ENOSYS`), while
  threads, `fork` and `ptrace` of its own children work;
- startup: the benchmark above;
- vm: a micro-VM step loop-mounts a disk as root, returns its exit code and
  outputs, and leaves its copy-on-write sources unchanged on the host. It
  needs `/dev/kvm`, QEMU, `mkfs.ext4` and `TILO_VM_GUEST`, a directory holding
  guest's `kernel` and `busybox`.
