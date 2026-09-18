# Make one change cheap to test

Date: 2026-09-18. Input: the user's ARM64 bring-up development transcript.
This changes roadmap priorities; it does not modify the ongoing board bring-up,
production workflows or main. Source observations refer to baseline `032ea136f`.

## What the transcript changes

The primary outcome is **elapsed time from requesting a small change/test to a
useful result**. Compiler-cache hit rate, time to merge and green CI are supporting
metrics. A fully cached compile is insufficient if assembly, artifact transfer,
worker preparation and signing handoffs still take most of the iteration.

Prioritize retaining a usable baseline and shortening assembly/test setup before
a broad build-system migration. Continue the scheduler and dependency-graph
qualification in parallel with these improvements; choosing Bazel is not a
prerequisite to reusing a retained image for installer development.

The transcript reports roughly minute-long small image-layer steps and manual
coordination overhead. Those are useful observations from that run, not a measured
distribution for the current fleet. Capture a current representative trace before
assigning percentages or predicting speedups.

## Existing pieces to consolidate

| Need | Existing implementation | Remaining work |
| --- | --- | --- |
| Shallower rootfs assembly | Enumerated scratch-stage overlays in `os/Containerfile` | Measure the residual snapshot/compression/export cost; extend only demonstrated hot boundaries |
| Registry cache without duplicating all intermediate Rust layers | `build-image.yml` already uses BuildKit `mode=min` | Measure local persistent builder storage separately; do not assume changing this flag is a new fix |
| Development assembly retry cache | `os/dev/assembly-registry-cache.md` describes the Podman path | Distinguish this path from BuildKit; current source epoch and fresh public trust limit cross-revision hits |
| Reuse compiled unsigned image through finalization | `retain-development-rootfs.py`, finalization driver and worker | Automatic artifact discovery/admission, retained evidence, resumable supported invocation |
| Installer media without recompiling apps | `os/docs/development-installer-media.md` | A retained admitted raw rootfs is the input; automate this existing route and prove an actual install cycle |
| Selected binary/development session iteration | `dist/dev/fedora44-vm-iterate.sh` | Reuse its behavior where applicable; do not assume the Fedora host loop qualifies a new ARM board |
| RPM input extraction | `derive-rpm-package-set.py` | Compare effective snapshot inputs before expensive work instead of treating every recipe edit as new package demand |

Source presence is not operational acceptance. The transcript says a useful known-
good complete image is still missing for that run. First discover and verify one,
or complete the current candidate and retain it; do not invent a baseline.

## Revised first implementation sequence

1. **Complete the current bring-up without redesigning its running pipeline.**
   Retain the first verified development baseline by immutable digest, including
   its admitted raw/unsigned input, finalized test medium and restorable VM state.
   Record source/helper/toolchain, architecture, board/kernel/firmware and trust
   identities, test evidence and retention leases. VM evidence and physical-board
   evidence are separate capabilities; a baseline is not universally good.
2. **Measure one warm change end to end.** Extend existing receipts with a common
   request ID and phase spans: planning/admission, queueing, environment setup,
   cache fetch, compile/link, snapshot, compression, registry export/import,
   finalization/signing wait, disk creation, VM restore/boot and tests. Record bytes,
   CPU time where observable, peak disk, retries and manual handoffs. Phase spans
   can overlap: report the critical path, not the sum as wall time. Distinguish
   service time from waiting time.
3. **Ship a supported targeted-test entry point over existing helpers.** First
   support installer tests against a retained image and desktop tests against a
   restored development VM. Select and pin the baseline once per request. Explain
   reused/rebuilt artifacts and the concrete reason for every rebuild. Return one
   result/receipt with logs and missing capability information. Proposed commands
   such as `tilo dev test installer --from <digest>` are design sketches, not
   existing commands. Agents and CI should call this same supported path.
4. **Remove demonstrated assembly and artifact overhead.** Compare warm persistent
   builder storage with registry-only recovery; benchmark snapshot/compression/
   upload independently. Group stable files at meaningful boundaries while keeping
   explicit payload allowlists. Keep changing trust/signatures late where the
   actual authenticated dependency order permits. Reuse unchanged RPM snapshots
   and compatible payloads, and provision test capacity early enough to overlap
   assembly without holding an idle expensive worker indefinitely.
5. **Keep continuous packaging and regular clean acceptance.** Successful task-
   scoped tests make development faster; periodic full builds, independent release
   reproducibility, installation/upgrade/recovery and real hardware checks still
   qualify release candidates. OS version promotion remains manual.

Do not remove checks or key binding to improve these timings. Move deterministic
missing-file/package/architecture checks before expensive assembly, using the real
intermediate image wherever host-only validation is insufficient.

## Small-change contract

| Change | Fast useful test | Reused inputs | Additional acceptance when relevant |
| --- | --- | --- | --- |
| App/desktop code | Scoped build/tests, then run in disposable development session | Compatible base, kernel, installer environment | Current runtime cohort and packaged launch; installed/boot test for changes affecting those boundaries |
| Installer logic | Storage fixtures and VM installation cycle | Admitted system payload | Rebuild required installer overlay, re-finalize signed media and verify actual installed result |
| System configuration | Syntax/policy/runtime preflight and affected assembly | Unchanged compiled payloads | Boot/service test if config changes startup behavior |
| Board firmware/profile | Affected profile/media and board boot test | Compatible generic/rootfs components | Physical-device evidence; VM success is insufficient |
| Signing/finalization helper | Finalization and verification against admitted retained raw input | Compiled image/payload | Fresh request/trust binding and signatures; prior finalized media is not raw input |
| Kernel | Kernel build and affected boot tests | Unchanged apps and other compatible components | Board/driver acceptance and recovery paths |

An overlay test reports exactly the modified artifact set. It does not certify the
baseline's original signed digest as containing the new code. Reset disposable VM
state between tests and retain the binding between test result, overlay and base.
Boot-chain tests use the normal authenticated finalization route.

Useful artifacts survive unrelated optional integration failures. For example, an
AirPlay failure can leave an installable candidate available for installer work
with that failure recorded. It still blocks a release whose policy requires that
capability; artifact retention and release acceptance are separate decisions.

## One front door, without a second CI engine

Automate current helper revisions, receipts, worker identities, signing requests
and admission checks behind one versioned interface. Record original producer
provenance and consumer evidence. Unknown compatibility fails with the required
rebuild and its reason; it does not create another hand-maintained exception chain.
Keep local development keys with the existing local signer and release keys in
their existing protected authority. Resume by reconciling completed artifacts and
request state, never blindly replaying a signing request.

Resource scheduling needs an existing owner. Use bounded pools/leases and pinned
tools images. Account for shared backing disk across assembly, finalization and VM
jobs; retain caches within budgets, clean only job-owned scratch, and protect active
baseline dependencies from garbage collection. A disconnected agent must be able
to reconnect to request status without starting the expensive work again.

This makes the Forgejo pilot more concrete: test native ARM worker capabilities,
persistent builder storage, late signing, resumable finalization, test-worker
preparation and cancellation without artifact loss. If those require a custom
general scheduler, evaluate Buildbot/Buildkite early. The developer-facing commands,
artifact contracts and helper entry points should survive that executor change.

## Added experiment and acceptance evidence

`experiments/ci-build-graph/assembly_inputs_probe.py` calls the production RPM-root
extractor on temporary Containerfile variants. Adding a configuration-only COPY
changes the recipe digest but leaves the direct RPM inputs unchanged: 174 x86-64
roots and 184 ARM64 roots. Adding an ARM64-only package marker changes only ARM64
selection (185 roots). No image or package snapshot is built.

This proves the existing extractor can support a finer preflight decision. It does
not prove complete snapshot equivalence: the eventual key must also cover pinned
base, repository metadata/resolution state, architecture, resolver/tool versions,
dependency/weak-dependency policy, snapshot recipe and relevant external assets.
Signing/acceptance evidence can need renewal without rebuilding RPM payload bytes.

Acceptance for the first developer-experience slice: an installer-only fix reaches
a real disposable VM install result without compiling apps/browser/kernel; a
desktop-only fix reaches its scoped test without recreating installation media;
reconnecting after controller/client interruption resumes the request; results
show every rebuild reason and unqualified capability. Measure cold, warm and
failure-recovery cases and both human/agent handoffs. Set time budgets from those
traces rather than promising a speedup from cache-hit percentage alone.
