# Measured resource budget and checkpoint

The **163.951 GiB (~164 GiB) free-disk admission threshold is a conservative scheduling budget, not a measured minimum size of the remaining CEF build**. It is the following sum on the existing 1,999.023 GiB filesystem:

| Component | GiB | Basis |
| --- | ---: | --- |
| Free space below which the operator's 95% critical watermark is crossed | 99.951 | Actual filesystem size and existing operator policy |
| Task recovery margin | 16 | Chosen reserve; not a measured build requirement |
| Reaction margin above the recovery floor | 16 | Chosen reserve for monitoring/termination while shared jobs continue writing |
| Additional owned output and link scratch allowance | 32 | Conservative per-tranche estimate/cap; remaining production peak not yet measured |
| Admission total | **163.951** | 176,041,230,336 bytes |

The running stop threshold is **131.951 GiB** (141,681,491,968 bytes): the first three rows. Neither threshold reserves space against other jobs. The 32 GiB allowance is free space in addition to the existing output; it does not double-count the retained 4.955 GiB. The production A/B runs are sequential; B retires only the verified task-owned output after A's package is protected, so this budget does not require two full output trees simultaneously. Packaging has a separate 8 GiB owned-growth cap and the same global disk/recovery guard.

Actual owned-output measurements are much smaller so far:

| Remote stage | Owned output at finish | Growth during stage | Aggregate process RSS peak |
| --- | ---: | ---: | ---: |
| Serial production calibration (PGO2 + ThinLTO, three patched C++ files and dependencies) | 4.107 GiB | Previous 12.717 GiB qualification output retired; not a comparable growth sample | 5.762 GiB |
| Production A, first tranche (five jobs) | 4.671 GiB | 0.565 GiB | 3.841 GiB |
| Production A, resumed tranche (five jobs) | 4.955 GiB | 0.284 GiB | 3.398 GiB |

The first production tranche completed approximately 9,270 of 53,476 actions; the resumed tranche completed another 2,898, leaving approximately 41,308. Action count is not proportional to future disk or memory requirements. The principal `libcef.so` ThinLTO link and its cache/scratch, the remaining large translation units, and completed-package peak have **not** been measured. Thus 32 GiB cannot be described as an observed need, and 4.955 GiB cannot be extrapolated into a demonstrated final peak.

During the first production tranche shared free disk fell 130.983 GiB while owned output grew 0.565 GiB; during the second it fell 35.489 GiB while owned output grew 0.284 GiB. Both stops were the global shared-disk guard, not the owned 32 GiB cap or a compiler failure. Both private containers were removed with no live session/namespace processes remaining. All completed objects and proof receipts remain retained.

## Memory and concurrency

The serial **5.762 GiB measured aggregate process RSS** was obtained with the real PGO2/ThinLTO profile; it is not a measurement of every remaining compilation or of the principal link. The preflight uses:

```
required currently available RAM = 64 GiB host reserve
                                + 30 GiB separate link allowance
                                + jobs × 2 × 5.762 GiB measured serial RSS
```

This requires **151.621 GiB for five jobs**, **140.097 GiB for four**, or **105.524 GiB for one**. The multiplier of two and 64 GiB are task reserves. The 30 GiB is a conservative allowance based on pinned Chromium's Linux ThinLTO link-pool heuristic, not an observed CEF link peak (the upstream argument is expressed as `--mem_per_link_gb=30`; this supervisor conservatively budgets 30 GiB). The five-job runs' lower actual peaks do not establish the memory requirement of later jobs. A fresh available-RAM measurement must select/refuse concurrency; the earlier five-job admission is not perpetual authorization for five jobs.

The task leaves eight CPUs unallocated in its instantaneous utilization calculation. Ninja and the linker's configured eight threads share the admitted aggregate hard cgroup CPU quota; the two production tranches had a 13-CPU quota and five Ninja jobs. Containers also enforce the freshly calculated memory limit, prohibit swap and continuously measure free disk and available RAM. No laptop compilation or parallel A/B build is permitted.

## What would justify a different capacity requirement

A read-only audit can establish whether ordinary shared jobs finishing will restore capacity, and which already-supported inputs/outputs contribute actual space. A smaller compile-only tranche could have a smaller admitted owned-growth estimate **if** its target closure cannot enter the unmeasured principal link and its output/generator/tool-link peaks are bounded. The principal link and packaging would still require their own measured or conservatively justified admission. Merely reducing the 164 GiB number to fit current free space is not evidence.

This document does not request cleanup, change operator policy, authorize a larger builder or revise the current guards. The rejected shared-GC action remains unperformed. `resource-evidence.json` records exact bytes, measurement tokens, original receipt hashes and verified cleanup. Complete original receipts/logs and the continuation state are retained at `/tmp/tilo-cef-renderer-repair` locally and in the existing task-owned remote scratch root.
