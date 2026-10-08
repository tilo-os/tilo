# Measured remote resource budget

Compilation and testing use the existing remote Cargo/SSH builder. Eight compiler jobs and all their subprocesses share an eight-CPU hard cgroup quota. The memory cap is 100 GiB, swap is prohibited, and fresh admission requires at least 164 GiB available RAM: the workload cap plus a 64 GiB host reserve. Twenty-minute windows continue only after verified cleanup, fresh admission and checks for compiler failures or memory-limit events.

On the existing 1,999.023 GiB filesystem, compile admission requires 176,041,230,336 bytes (163.951 GiB) free:

| Component | GiB | Basis |
| --- | ---: | --- |
| Existing 95% critical watermark | 99.951 | Actual filesystem size and operator policy |
| Recovery margin | 16 | Task reserve |
| Monitoring and termination margin | 16 | Task reserve while shared jobs write |
| Additional owned output | 32 | Conservative estimate/cap per window |

This is a scheduling allowance, not a measured minimum build size or an exclusive reservation. Runtime disk and memory monitoring can stop a window safely. The running compile stop threshold is 141,681,491,968 bytes free. No shared GC or cache purge is authorized.

## Actual measurements

`resource-evidence.json` binds tokens, complete receipt hashes, execution scopes and exact memory/cleanup counters for these collected measurements:

| Stage | Cgroup memory peak, bytes | Scope |
| --- | ---: | --- |
| Largest previous-source compile window among eleven collected windows | 19,962,945,536 | Eight CPUs; 100 GiB cap |
| Previous-source principal ThinLTO link | 16,482,787,328 | Separate eight-CPU link admission |
| Previous-source complete runtime resources | 2,091,081,728 | Eight CPUs |
| Previous-source SDK packaging | 940,933,120 | Separate packaging cap |
| Current prior-source 165-controller-test build, first window | 10,046,586,880 | Eight CPUs; 100 GiB cap |
| Previous-source actual CEF fixture | 412,610,560 | Two CPUs; 8 GiB cap |

Every listed receipt has zero high/max/OOM events and no surviving owned processes. The first current controller build window completed 20,393 of 42,278 actions and produced 5,618,712,576 bytes of owned output. Action count is not proportional to future memory or disk requirements.

The previous source completed SDK A and its package, but failed two of sixteen unchanged real CEF assertions. Its artifact and the stopped 9,302,310,912-byte partial B output are preserved as evidence, not qualified dependencies. These measurements do not certify the revised renderer or its future SDK.

## Link, package and native qualification admission

The principal production ThinLTO link remains excluded from compile windows. Its separate trial uses one Ninja job, eight LLD threads under the aggregate eight-CPU quota, a 128 GiB hard memory cap, a 64 GiB host reserve, and fresh admission of at least 192 GiB available RAM. Its 64 GiB owned-growth allowance is conservative; the previous-source measured link peak does not establish the revised link's requirement.

Packaging uses the pinned public CEF `make_distrib.py`, an 8 GiB memory cap and separate disk-growth monitoring. It rejects partial distributions and preserves all public headers, resources, locales, wrapper sources and licenses.

Real CEF tests use the owner's documented `bwrap --unshare-all --proc /proc` fixture directly on the existing builder, with no hardware or external network. A token-bound existing systemd user service supplies a two-CPU quota, an 8 GiB memory cap, no swap, a runtime deadline and cgroup-wide cleanup. This avoids nested Podman's proc-mount restriction while retaining Chromium's sandbox. Combined admission reserves the running 100 GiB CEF workload, the 8 GiB native fixture and the 64 GiB host reserve; no host sandbox policy, capability or signing authority is changed.

The supervisor holds the existing task/admission locks, maintains monitored activity, and verifies empty owned process scopes and removed ephemeral containers/services before advancing. The rejected earlier shared-GC operation remains unperformed. Fresh capacity must be measured for every next stage; existing receipts never grant perpetual admission.

## Completed revised SDK measurements

The cold B compile completed in eight original bounded windows; its largest charged compile peak was 17,348,923,392 bytes. B principal link completed in 396.076 seconds at 16,709,951,488 bytes under the unchanged eight-CPU/128 GiB cap and 64 GiB reserve. Full B resources took 50.077 seconds at 2,221,326,336 bytes. Packaging with the public entry point took 40.550 seconds at 961,818,624 bytes under two CPUs/8 GiB. One package admission refused 63.53 busy CPUs before any producer; a separate five-second measurement showed 37.199 busy CPUs, and the unchanged original gate subsequently admitted packaging. No limits or reserves were weakened.

A/B archives and all 960 inventory entries matched exactly. Actual mounted replay passed 48 scenarios at 873,357,312 bytes; the actual normal offline reader passed at 838,516,736 bytes. All completed scopes have zero memory-limit/OOM events and verified cleanup. These are measured peaks, not permission to waive fresh admission. Publication and production signed Browser proof remain pending live publisher namespace verification and the held normal integration path.
