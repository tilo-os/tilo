# Guided migration: current feature status

Status reviewed: 2026-09-22 UTC.

**The complete Windows/Linux migration experience is not ready.** The guided
setup screens and several underlying services exist, but USB-free Full Tilo
installation and Connected folders remain disabled. A download, source test,
or successful build does not establish a working installation on a real host.

This page covers Imager's `--setup` and `--import-folders` flows. The existing
[removable-media installer](installer-walkthrough.md) and
[manual Linux desktop packages](packaging.md) have separate release and
acceptance requirements.

| Feature | Current status | What the user can expect |
| --- | --- | --- |
| Choose Desktop or Full Tilo, select folders, and review storage | Implemented selection and review UI | Choices are capability-gated. Copy is the default folder policy; each folder can have its own policy, or be left for later. Selecting a plan does not install an OS or copy files. |
| Add Tilo Desktop to Linux | Implemented host/package flow; release-gated | Host discovery, verified package download, privileged installation, session verification, and first-login folder review exist. The checked-in Desktop catalog is empty; guided installation needs an authenticated matching release with native graphical-session evidence. This does not certify every distribution supported by manual packaging. |
| Add Tilo Desktop to Windows | Unsupported by design | Desktop-only setup is a Linux option. The intended Windows migration route is Full Tilo alongside Windows, which remains unavailable below. |
| Download, mount, and boot Full Tilo without a USB drive | In development; unverified on real hardware; off in release builds | The [USB-free design](superpowers/specs/2026-09-22-usb-free-install-design.md) is being built. Setup now has the Full Tilo review: a host check off the UI thread, a plan showing how much the system partition shrinks (Windows) or which free space is used (Linux), a Tilo size stepper, blockers with remedies (restart to firmware settings for Secure Boot, turning off Fast Startup), the existing verified download, and an Install step that hands the verified download to an elevated staging helper, then offers Restart now / Restart later. An unfinished staging record gets a resume card (Try again / Remove). The staging helper, host probe and planner, boot handoff and the installer's same-disk mode are separate workstreams still landing; until they do, the live host check reports the disk as unsupported and nothing is staged. Full Tilo is offered only when the host check passes, the catalog has an internal release for this processor, and the build was made with `TILO_IMAGER_INTERNAL_INSTALL=1` (set only by release jobs whose VM round trip passed) or a development run sets `TILO_IMAGER_ALLOW_INTERNAL_INSTALL=1`. No release build sets it yet, and no qualified internal-installation bundle exists. Mounting an ISO alone does not provide an installer path. |
| Keep the existing OS and add dual boot without USB | In development; unverified on real hardware | The design keeps the existing OS installed and its firmware boot order unchanged: staging arms a one-time `BootNext` into the Tilo installer, the installer installs into the reviewed free space on the same disk, and the retained OS stays the startup default. Leaving the installer returns to the existing OS, where setup offers Try again or Remove. None of this has been proven on a real Windows or Linux computer; the Windows path has no destructive round trip at all yet. The separate removable-media installer has an alongside path. |
| Copy selected folders once | Implemented copy/import service; platform limits remain | The import UI uses a real worker with verified copies, resumable receipts, and explicit failures, preserving originals and existing destination content. Windows journaled copying is currently refused because native directory-entry durability is unfinished. Full-OS copying after reboot remains pending until target-side source identities and destinations can be resolved and reviewed. |
| Keep folders Connected | Incomplete; disabled in live setup | Comparison, identity, transaction, and recovery foundations exist. The complete user-facing synchronization service and native Windows/Linux qualification are unfinished; current native endpoint gates refuse general writable sync. A selected folder or saved record does not enable syncing. A driverless structural framing prototype (`tilo-connected-protocol`, `runtime-connected`) was removed in TIL-360; recover it from commit `c74ad6c1bf`. |
| Finish the complete migration on Windows or Linux | Unverified end to end | Host tests and individual platform implementations cover parts of the process. They do not establish download → internal install → both OS boot paths → verified copying or synchronization on either platform. |

Connected folders are designed as synchronized local copies. Changes would be
reconciled when both locations are available, including between dual-boot
sessions; they are not direct writable links to the original folders. Selecting
folders grants no collaboration-sharing or cloud-upload permission.

Before enabling the unfinished features, delivery still requires:

- A real installer image with authenticated provenance, a completed schema-2
  exporter, the elevated staging helper, the installer's same-disk mode and
  staging reclaim, and a passing VM round trip (stage, boot the installer,
  install alongside, boot Tilo, reclaim, cold-boot the retained OS with its
  boot order unchanged) before any release build sets
  `TILO_IMAGER_INTERNAL_INSTALL=1`.
- Native Windows and Linux acceptance for installation interruption and recovery,
  preservation of existing partitions, both boot paths, and the retained startup
  default on every advertised configuration.
- Completed synchronization orchestration and recovery, native filesystem
  qualification, and reviewed first-login copying across the Full-OS reboot.

The [September 22 installer-release run](https://code.tilo.so/tilo/tilo/actions/runs/17758)
still refused to build installation media because it had no signed,
digest-pinned multi-architecture OS image. The earlier
[successful ARM development finalization](https://code.tilo.so/tilo/tilo/actions/runs/12058)
does not satisfy that release input or establish USB-free boot support.

The pending Connected history implementation is also failing its maximum
4,096-step Linux ext4 replay test. The real 4,095-step prehistory and its
empty-log reopen pass, but replaying the terminal events consumes the existing
60-second read lease before the mandatory final history closure. A diagnostic
run saw the three events consume about 14 seconds each after an initial
16-second closure; roughly 949 MiB of the 1 GiB read allowance was already
charged. Experimental row and review caches did not solve the repeated
physical baseline reads and were not accepted. Do not enable Connected until
both maximum completion and step-limit cases pass replay, checkpoint, and
reopen without changing the finite limits or weakening physical proof reads.

The live availability gates are in
[`setup/view.rs`](../apps/imager/src/setup/view.rs) and, for Full Tilo, in
[`setup/internal_install_flow.rs`](../apps/imager/src/setup/internal_install_flow.rs);
Desktop qualification is in
[`desktop_probe.rs`](../apps/imager/src/setup/desktop_probe.rs) and the
[`Desktop catalog`](../apps/imager/desktop-catalog.json). The export refusal is
in [`internal-bundle.py`](../os/dev/internal-bundle.py). The
[approved migration design](superpowers/specs/2026-09-16-guided-migration-design.md)
describes the intended experience, not additional delivered capabilities.
