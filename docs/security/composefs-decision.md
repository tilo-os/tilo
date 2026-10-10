# Will bootc's composefs backend stabilise by ~February 2027?

Researched 2026-08-11 against bootc 1.16.7 (latest release) and bootc `main`.
Tilo currently pins bootc 1.16.4 and `quay.io/centos-bootc/bootc-image-builder:latest`.

## Verdict

**A formal stabilisation — "experimental" dropped and the on-disk format declared
frozen — is unlikely by February 2027. Confidence: moderate-high that it does not
happen (I'd put it around 30-35% that it does).**

The reason is narrow and checkable: bootc cannot declare its on-disk format stable
ahead of the format it is built on, and composefs-rs still says in-tree, today,
"At this time, the composefs-rs repository format is not declared stable."
There is no bootc milestone, no dated target, and no live tracking issue for
composefs stabilisation — the tracking issue that existed (#1190) was *closed* in
January 2026 with work items still unchecked. What replaced it is a docs page and a
release cadence, not a plan with a date.

**However, the decision should not hinge on that verdict, and the stated
decision rule ("real chance of stabilisation → adopt; otherwise restore GRUB")
rests on a premise the evidence contradicts.** Two findings drive that:

1. **bootc has already made, and is already honouring, the specific guarantee that
   a stabilisation label would give us.** The docs say: *"We are committed to
   in-place upgrades from all systems deployed since bootc 1.16.0."* Tilo is on
   1.16.4 — inside that window. And this is not aspirational: release 1.16.5
   shipped `store: Fix upgrade with existing V2 composefs repo`, i.e. a repository
   format migration was handled in place rather than by reinstall.

2. **Waiting is the option that costs a reinstall, not adopting.** ostree →
   composefs migration does **not** exist and is not close (#2079 open;
   the docs list it as blocked behind factory-reset support, #404). A machine
   installed on the ostree backend today **cannot** be moved to composefs without
   reinstalling. A machine installed on the composefs backend today is covered by
   the 1.16.0+ in-place commitment. The usual "wait for stable" instinct is
   inverted here.

Restoring GRUB also costs more than systemd-boot. Tilo's TPM rollback protection
already structurally depends on a UKI — `os/tests/rollback-boot-check.sh` asserts
`tilo.generation=` is on the *measured* kernel command line and checks PCR 11 /
`StubPcrKernelImage`, with the comment "It has to come from the UKI, not from a
bootloader config". The ostree backend cannot produce that. So the real
alternative to composefs is not "GRUB instead of systemd-boot", it is
"abandon the measured-boot rollback binding already built into the tree".

My recommendation, stated separately from the factual verdict: **adopt the
composefs backend, but budget for replacing bootc-image-builder in the disk-image
step, because bib cannot select the backend at all and that request has been
stale for eight months.**

---

## Evidence

### The premise checks out: ostree can never do systemd-boot

Confirmed on `main`, not just 1.16.4. In
[`crates/lib/src/install.rs`](https://github.com/bootc-dev/bootc/blob/main/crates/lib/src/install.rs):

```rust
Bootloader::Systemd | Bootloader::GrubCC => {
    anyhow::bail!("bootupd is required for ostree-based installs");
}
```

There is no alternative path for `Bootloader::Systemd` under ostree.
[`docs/src/bootloaders.md`](https://github.com/bootc-dev/bootc/blob/main/docs/src/bootloaders.md)
states systemd-boot "requires the Composefs Backend and is unavailable for
Ostree", and that if bootupd is absent from the image, systemd-boot is used by
default (except s390x).

### The gate has already been removed

`--composefs-backend` is **not** hidden and **not** feature-gated — it is a plain
clap bool in `InstallComposefsOpts`:

```rust
#[clap(long, default_value_t)]
#[serde(default)]
pub(crate) composefs_backend: bool,
```

This matters because bootc *does* gate things it considers unstable, in the same
file and the same release: `unified_storage_exp` carries an `_exp` suffix, and
1.16.0's notes introduce a `BOOTC_EXP_EXTERNAL_CONTAINER_TOOL` env override.
composefs carries neither. It is documented in the shipped man page
([`bootc-install-to-disk.8`](https://bootc.dev/bootc//man/bootc-install-to-disk.8.html))
with no experimental warning attached to the flag.

### What bootc itself says — and the tension in it

From [`docs/src/experimental-composefs.md`](https://github.com/bootc-dev/bootc/blob/main/docs/src/experimental-composefs.md)
([rendered](https://bootc.dev/bootc/experimental-composefs.html)), both of these
appear in the same file:

> **Status**: Experimental, but close to stabilization! We are committed to
> in-place upgrades from all systems deployed since bootc 1.16.0.

> ### Known Issues
> The composefs backend is experimental; on-disk formats are subject to change.

The honest reading is not "the format is frozen". It is "the format may still
change, and we intend to migrate you across it rather than break you". Those are
different guarantees, and the second is the one that is actually load-bearing for
an installed base.

Also still listed as a live stopgap in that same Known Issues section:

> [Dual EROFS v1/v2 generation](https://github.com/bootc-dev/bootc/pull/2248):
> `bootc install` currently generates both EROFS v1 and v2 images for every
> deployment [...] this dual generation is a stopgap until we can drop v1 support.

An admitted stopgap in the on-disk image generation is direct evidence the format
has not settled.

### Whose format is unstable: composefs-rs, not just bootc

The warning traces upstream. composefs-rs'
[`repository_format.rs`](https://github.com/composefs/composefs-rs/blob/main/crates/composefs/src/repository_format.rs)
states plainly: **"At this time, the composefs-rs repository format is not
declared stable."**

It is not unstructured, though. The repository carries a `meta.json` with an
integer `version` (currently `1`), tools "must refuse to operate on a repository
whose version exceeds what they understand", and there is a three-level feature
flag model — `compatible` (safe to ignore), `read-only-compatible` (old tools may
read, not write), `incompatible` (refuse entirely). Three EROFS format
generations are defined (V0 plain, V1 current default, V2 legacy/original), with
`v1_erofs` flagged `read-only-compatible`.

That is an ext4/btrfs-style compatibility mechanism. It means a format change
produces a *refusal or a migration*, not corruption. This is the single biggest
reason I think the on-disk-format risk is smaller than the word "experimental"
implies.

### Velocity: high and sustained, but still finding structural gaps

composefs work appears in essentially every release from 1.15.1 (2026-04-14)
through 1.16.7 (2026-08-04):

| Release | Date | composefs-relevant note |
|---|---|---|
| 1.16.7 | 2026-08-04 | `packaging: Drop shim alongside bootupd when switching to systemd-boot` |
| 1.16.6 | 2026-07-28 | `bootloader: Chroot into composefs image root for bootupd installs` |
| 1.16.5 | 2026-07-24 | `store: Fix upgrade with existing V2 composefs repo` |
| 1.16.4 | 2026-07-15 | `composefs/bls: Add user provided kargs`; `UKI Cleanup` |
| 1.16.3 | 2026-07-02 | `Support uki key in BLSConfig` |
| 1.16.0 | 2026-06-10 | `composefs: Transient /etc, transient root, volatile /var`; composefs manifest is now a GC root |
| 1.15.2 | 2026-05-01 | `composefs/uki: Remove stripping .staged suffix on finalize` |

Turnaround on composefs bugs is days (#2364 filed 2026-08-04, fixed by #2365
merged 2026-08-05). That is a healthy, actively-driven feature.

But the ~20 open composefs issues are not all polish. Several are missing
subsystems:

- [#2079](https://github.com/bootc-dev/bootc/issues/2079) Migrating from ostree to composefs backend — **open**
- [#1886](https://github.com/bootc-dev/bootc/issues/1886) `bootc install reset` not implemented for composefs
- [#2336](https://github.com/bootc-dev/bootc/issues/2336) soft-reboot not implemented for composefs
- [#2208](https://github.com/bootc-dev/bootc/issues/2208) a series of rebases breaks bootc rollback, **persisting through reboots**
- [#2334](https://github.com/bootc-dev/bootc/issues/2334) upgrade with composefs+UKI: `composefs=` parameter mismatch
- [#1976](https://github.com/bootc-dev/bootc/issues/1976) UKI profiles ignored with `install to-disk --bootloader=systemd --composefs-backend` — *exactly Tilo's flag combination*
- [#2131](https://github.com/bootc-dev/bootc/issues/2131) / [#2132](https://github.com/bootc-dev/bootc/issues/2132) `install to-disk` builds the wrong partition layout for composefs+UKI (BIOS partition created, ESP too small)
- [#2375](https://github.com/bootc-dev/bootc/issues/2375) status/upgrade/switch fail on non-EFI BLS systems (filed 2026-08-10, against 1.16.7)

Note that #2208 and #2334 are the field-risk cluster: state that breaks across
*updates*, which is precisely what a long-lived installed base does.

### No milestone, no date

bootc's [milestones](https://github.com/bootc-dev/bootc/milestones) are `1.17`
and `1.18`, both open, both with no description and no composefs stabilisation
issue attached. The composefs-native tracking issue
[#1190](https://github.com/bootc-dev/bootc/issues/1190) is **closed** (updated
2026-01-29) with items still outstanding (GC, tmt tests under composefs,
`bootc storage init-composefs`, cross-distro CI, UKI docs). There is no artefact
anywhere in the project that commits to a stabilisation date.

Strategic direction is clear but undated. In
[discussion #540](https://github.com/bootc-dev/bootc/discussions/540), cgwalters
frames composefs as the unification point while being explicit that ostree is not
going away — "We shipped ostree in at least RHEL9 and need to support it for quite
a long time" — and describes the transition timing as murky.

### Distro adoption: real, and further along than expected — but explicitly test-grade

This is the strongest positive signal, and it is worth reading carefully because
two different "Fedora composefs" things are easy to conflate.

- **Not this**: [Changes/ComposefsAtomicDesktops](https://fedoraproject.org/wiki/Changes/ComposefsAtomicDesktops)
  (accepted, F42) enables composefs *inside the ostree backend*. It is not the
  composefs-native backend and should not be counted as evidence for it.
- **This**: [Sealed Fedora Atomic Desktop bootable container images](https://fedoramagazine.org/sealed-atomic-desktops-test-images/)
  (Fedora Magazine, 2026-04-28; also [Siosm's blog](https://tim.siosm.fr/blog/2026/04/28/sealed-atomic-desktops-test-images/)),
  by a Fedora Atomic Desktops lead. These use the composefs-native backend with
  systemd-boot + UKI + Secure Boot, x86_64 and aarch64, UEFI only. Covered in
  [What's New for Fedora Atomic Desktops in Fedora Linux 44](https://fedoramagazine.org/whats-new-fedora-atomic-desktops-in-fedora-linux-44/).

The caveats are load-bearing: *"Beware, those are testing images."* Unofficial,
components not signed by Fedora, no installation ISO, no Anaconda support yet,
root has no password and sshd is on. Repo:
[travier/fedora-atomic-desktops-sealed](https://github.com/travier/fedora-atomic-desktops-sealed)
— self-described "Work in progress, unofficial development images".

Separately, [projectbluefin/dakota](https://github.com/projectbluefin/dakota) is a
distro shipping pure bootc + composefs with no ostree and systemd-boot, but it is
alpha and is filing exactly the bugs listed above (its issues #531 and #665 are
the "Multiple extra entries in /boot" rollback failure).

**No shipping distro has committed to defaulting to the composefs backend.** That
is the commitment that would settle this question, and it does not exist yet.

---

## What adopting composefs today actually costs

### 1. bootc-image-builder cannot do it — this is the largest concrete cost

bib's [README](https://github.com/osbuild/bootc-image-builder/blob/main/README.md)
does not mention composefs, sealed images, UKI, or backend selection **anywhere**.
Its documented config surface is `customizations.{user,kernel,filesystem,installer,iso}`
and its flags are `--type`, `--config`, `--rootfs`, `--target-arch` and similar.
There is no key that reaches the backend.

[osbuild/bootc-image-builder#1164](https://github.com/osbuild/bootc-image-builder/issues/1164),
"Support building bootc composefs backend image (sealed image)" — *"BIB can't build
bootc composefs backend image (sealed image)."* — is **open**, created 2025-12-03,
**last activity 2025-12-08**, assigned to mvo5, and locked. The four comments never
address composefs support; the thread diverted into a container-tag-detection bug
(`image localhost/bootc:latest is not a bootc image`). Eight months of silence on
the actual request.

The capability does exist one layer down: the osbuild stage
[`org.osbuild.bootc.install-to-filesystem`](https://osbuild.org/docs/developer-guide/projects/osbuild/modules/stages/org.osbuild.bootc.install-to-filesystem/)
has both a `bootloader` enum (`grub` / `systemd` / `none`) and:

```json
"composefs": {
  "description": "Use the compsefs backend instead of the ostree backend",
  "type": "boolean",
  "default": "false"
}
```

So bib's gap is plumbing, not a missing primitive. Two ways out:

- **Patch bib/osbuild-images** to surface the option. Small in principle, but it
  puts us on a fork or on upstream's eight-month-stale timeline.
- **Drop bib for the disk step**, which is what Fedora's own sealed images do:
  `systemd-repart` to partition, then `bootc install to-filesystem`. The sealed
  repo's README: *"Until we get support ready in Anaconda, we will use
  `systemd-repart` to partition the system and `bootc install to-filesystem` to
  install the sealed image."* Note also
  [bootc#2314](https://github.com/bootc-dev/bootc/pull/2314), which is bringing
  systemd-repart partitioning into `bootc install` itself and raising the default
  ESP to 2GB for composefs — so this path is converging on being built in.

Tilo uses bib in `os/build.sh`, `os/build-host.sh`, `os/verify-test.sh`,
`os/dev/build-image.sh`, `os/dev/build-installer.sh` and
`os/dev/build-custom-installer.sh`. This is the work item to size, and it is the
honest cost of the decision.

Worth noting: Tilo's `os/usr/lib/bootc/install/40-tilo.toml` already sets
`bootloader = "systemd"`, and its own comment claims bib drives the same install
code path. Given the `bail!` above, that config **cannot currently be producing a
systemd-boot install through bib** — it either errors or something is falling back.
That is worth verifying against a real build log independently of this decision.

### 2. The UKI requirement is softer than assumed — and Tilo needs a UKI regardless

Correcting the premise in the brief: **a UKI is not required for the composefs
backend.** Per the docs:

> Composefs installs using a traditional `vmlinuz`/`initramfs.img` layout instead
> of a UKI are always unsealed, and can use either `bootupd` (GRUB) or
> systemd-boot, the same as the ostree backend.

So plain vmlinuz + initramfs + BLS entries + systemd-boot on the composefs backend
is a supported configuration today. What the UKI buys is *sealing* (fsverity
enforcement bound to a signed digest).

The auto-selection claim is also narrower than "UKI required": *"Whenever the
container image has a UKI, bootc automatically selects the composefs backend
during installation."* That is UKI ⇒ composefs, not composefs ⇒ UKI. Without a
UKI you select it explicitly with `--composefs-backend`, which the docs flag as
*"not as heavily tested yet"*.

But Tilo does want the UKI, independent of composefs: the rollback design in
`os/tests/rollback-boot-check.sh` requires `tilo.generation=` on a *measured*
command line (PCR 11, `StubPcrKernelImage`). Building it means a three-stage
Containerfile:

```
rootfs  →  bootc container split-kernel-and-rootfs --rootfs / --output /kernel
        →  bootc container ukify --rootfs /target --kernel-dir /kernel/$kver -- \
             --output /out/$kver.efi \
             --secureboot-private-key ... --secureboot-certificate ...
        →  COPY the signed .efi to /boot/EFI/Linux/
```

The sketch is what got built, with one correction: Tilo does **not** sign at build
time. Its root of trust is a Platform Key the target machine generates for itself, so
there is no build-time key that machine's firmware would accept —
`/usr/libexec/tilo-secureboot` signs the UKI on the installed system instead.
Measurement does not depend on the signature.

A later refinement (TIL-733): in a layered build the split is a whiteout plus a
fresh copy at `/kernel`, so an image that ships `/kernel` carries the kernel and
initramfs a second time beside the UKI. `os/Containerfile` therefore splits twice
from the same rootfs: a throwaway `kernel` stage feeds the UKI, and the shipped
`split` keeps only `/kernel/<kver>/vmlinuz` (the signed development board restores
it from there) and drops the initramfs, which the UKI's `.initrd` carries byte for
byte. `tools/policy/check_uki_pipeline.py` holds the three sealed images to that
shape.

Two things fit Tilo unusually well here:

- `bootc container ukify` "reads extra kernel arguments from
  `/usr/lib/bootc/kargs.d`". Tilo already has
  `os/usr/lib/bootc/kargs.d/01-hardening.toml`, so the existing kargs mechanism
  feeds the UKI command line directly — which is where `tilo.generation=` now goes,
  as `02-generation.toml` written from the `TILO_GENERATION` build argument.
- `--uki-addon` exists for machine-specific kargs as separately signed PE addons,
  if per-machine cmdline additions are ever needed without rebuilding the UKI.

One real conflict to resolve: sealed images must have systemd-boot and **must not
have bootupd**. Tilo installs `systemd-boot-unsigned` (good) but
`os/usr/lib/systemd/system/tilo-secureboot-sign.path` and `os/usr/libexec/tilo-secureboot`
are built around `bootupctl update` rewriting the ESP. That machinery needs
rethinking under a sealed composefs image. (bootc 1.16.7's `packaging: Drop shim
alongside bootupd when switching to systemd-boot` is upstream moving in the same
direction.) — Resolved, and it was less than it looked: see the addendum.

### 3. The deployment-naming problem that blocked us before is not real

The concern was that a static UKI embeds a command line naming a deployment, and
deployment names change on every update. The mechanism resolves it:

- Deployments are **content-addressed**. `/state/deploy/<deployment-id>/` is named
  after the deployment's composefs digest (SHA-512, 128 hex chars).
- The UKI's embedded cmdline carries `composefs=<that same digest>`, computed at
  **build** time by `bootc container ukify` over the built rootfs.
- So the UKI names the *image content*, not a mutable slot. Each new image build
  yields a new digest and a new UKI, and the two ship together inside the same
  container image. Nothing needs to rewrite a signed cmdline at deploy time.
- For the unsealed non-UKI path the question does not arise at all: bootc writes
  standard BLS entries at deploy time and fills the digest in then.

Caveat, honestly: [#2334](https://github.com/bootc-dev/bootc/issues/2334) is an
open bug titled "Error during upgrade w/ composefs-backend and UKI: `composefs=`
parameter mismatch". The design is sound; the implementation has live bugs on
exactly this seam.

### 4. On-disk format risk on real machines

Ranked by what I'd actually worry about:

- **Low: repository format churn.** Versioned `meta.json` plus the
  compatible / read-only-compatible / incompatible flag model means mismatches
  produce refusals, not corruption. And bootc has already demonstrated an in-place
  repo migration (1.16.5, "Fix upgrade with existing V2 composefs repo").
- **Low-moderate: EROFS image format.** Still explicitly a stopgap (dual v1/v2
  generation) and expected to change when v1 is dropped. These images hold only
  metadata and are regenerated per deployment, so the blast radius is small.
- **Moderate — the one to actually plan for: boot-state and rollback bugs across
  update series.** #2208 (rollback breaks after a series of rebases, persisting
  through reboots) and #2334 are failures that accumulate over a machine's
  *lifetime*, not at install. For Tilo this is sharper than for most, because
  bounded rollback protection is wired into TPM-sealed LUKS keyslots — a broken
  rollback entry is not just an inconvenience, it interacts with disk unlock.
  This deserves a deliberate test: install, then rebase/update repeatedly, and
  assert rollback still resolves.
- **Not covered: `bootc install reset`** (#1886) and **soft-reboot** (#2336) are
  simply not implemented for composefs.

### 5. Migration in or out

| | Supported? |
|---|---|
| composefs (1.16.0+) → later composefs | **Yes** — explicit commitment, demonstrated in 1.16.5 |
| ostree → composefs, in place | **No.** #2079 open; docs list it behind factory reset (#404), then "support copying /etc and /var" |
| composefs → ostree | Not documented anywhere; assume reinstall |

This asymmetry is the crux. Installing on composefs today buys a supported forward
path. Installing on ostree today and waiting for stabilisation buys a **reinstall
of every deployed machine**, on a feature the project has not scheduled.

---

## What would change the answer

**The single resolving signal:** composefs-rs declaring its repository format
stable — i.e. the sentence "At this time, the composefs-rs repository format is not
declared stable" disappearing from
[`repository_format.rs`](https://github.com/composefs/composefs-rs/blob/main/crates/composefs/src/repository_format.rs).
bootc cannot stabilise ahead of its storage format, and once that lands, bootc's
remaining blockers are all bootc-side (migration, reset, soft-reboot, the rollback
bugs) rather than format questions. Watch that one file.

**Second-strongest:** a Fedora Change proposal making the composefs backend or
sealed images the *default* for Atomic Desktops. Fedora 44 shipped end of April
2026 ([The Register, 2026-04-29](https://www.theregister.com/software/2026/04/29/fedora-44-is-out-countless-versions-of-it/5224584)),
so F46 lands around April 2027 and its proposal deadline falls roughly
December 2026 – January 2027 — **inside the window**. If an F46 proposal appears,
stabilisation is effectively confirmed. If the F46 cycle passes without one, it
slips past February 2027. This is the checkable milestone to diary.

**Would also move it:** a `1.17`/`1.18` milestone acquiring a composefs
stabilisation issue; bib#1164 getting maintainer commitment; #2079
(ostree→composefs migration) landing, which would both remove the reinstall
asymmetry and signal the format is settled enough to migrate onto.

**Would reverse the recommendation:** #2208-class rollback corruption turning out
to be architectural rather than a bug, given how tightly Tilo binds rollback state
to TPM-sealed LUKS keyslots.

## Where this is thin

Stated plainly:

- **The 30-35% figure is my judgement, not a sourced number.** No source gives a
  probability or a date. It is an inference from "no milestone, no date, upstream
  format explicitly unstable, structural gaps still open" weighed against
  "close to stabilization", high velocity, and real distro use.
- **I could not confirm whether osbuild/images (the layer between bib and the
  osbuild stage) surfaces the `composefs` field.** The GitHub code-search API
  returned 401. I verified the stage schema has it and that bib's README does not
  expose it; the middle layer is unverified, so the bib patch could be smaller than
  I've implied.
- **bib#1164's comment thread was only partially retrievable.** I have all four
  comments' substance (achilleas-k ×2, cgwalters, mvo5) and none address composefs
  support or a timeline, but I cannot rule out discussion elsewhere, e.g. the
  referenced internal Red Hat ticket HMS-9844.
- **I did not build or boot anything.** Every claim about Tilo's own state comes
  from reading `os/` in this tree; the claim that Tilo's current bib path cannot
  actually be installing systemd-boot is an inference from the `bail!` and should
  be checked against a real build log.
- Release-note quotes come from the GitHub releases API rather than each release
  page read in full, so they are accurate but not necessarily complete.

---

## Addendum: measured against real builds

Two open items above are now settled by running them.

**Tilo's bib path could not install systemd-boot — it could not install at all.**
`bootc-image-builder` on the current desktop image fails after deploying the
container, at the bootloader step:

```
Bootloader: systemd
error: Installing to filesystem: bootupd is required for ostree-based installs
```

bootupd only plants GRUB, so `bootloader = "systemd"` and the ostree backend are
mutually exclusive, and bib has no way to ask for the other backend. The
inference from the `bail!` was right, and stronger than stated: the bib path was
not silently falling back to GRUB, it was not producing a qcow2. Replacing bib
was therefore not a cost of adopting composefs — the composefs backend is what
makes the bootloader Tilo already asks for installable.

The same image installs and boots through `systemd-repart` +
`bootc install to-filesystem --composefs-backend`. It first did so with no UKI:
systemd-boot on the ESP, a BLS entry pointing at vmlinuz + initrd there, and a root
mounted `composefs:<digest> overlay ro,…,verity=require`. So fsverity enforcement
arrived with the backend, before any of the UKI work — what the UKI adds on top is a
*signed and measured* command line, which is what binds `tilo.generation=` to PCR 11.

**The production image now builds the UKI**, in three stages at the end of
`os/Containerfile` (split → ukify → final; see
[boot-chain.md](boot-chain.md#building-the-uki)). Three things that cost time and are
not obvious from the sketch at the top of this file:

- **`bootc container ukify` needs a host-backed `/var/tmp`.** It builds a temporary
  composefs repository there and writing an object needs `O_TMPFILE`, which
  overlayfs — the filesystem a build stage's root is — does not support. The error
  is `Creating object tmpfile: … Operation not supported (os error 95)` and it names
  neither `/var/tmp` nor overlayfs. `--mount=type=cache,target=/var/tmp` fixes it and
  also stops the whole rootfs being re-hashed on every build.
- **The build has to run twice** until bootc#2334 is fixed, because
  `bootc container ukify` and `bootc install` compute different composefs digests for
  the same image. install's expected value is stable across a rebuild of the UKI, so
  sealing the value it asked for converges; `os/dev/build-image.sh` runs the loop and
  stops with an explanation if the value ever moves.
- **The install source has to be a registry.** The composefs pull rejects
  `containers-storage:` — including an image moved across with
  `podman save | sudo podman load` — with `Opening stream '': Creating new
  splitstream reader: Invalid splitstream content type`. `os/dev/disk-image.sh`
  starts a local registry for that reason rather than assuming one.

**Nothing can be layered on a sealed image**, and that is the cost the probe never
had to pay — it was one thin image with nothing derived from it. A derived image that
adds a single file has a different composefs digest from the one its inherited UKI
seals, so `bootc install` refuses it, correctly. Rebuilding the UKI inside the
derived image is not a way out either: the kernel and initramfs are no longer in the
rootfs to build one from, they are sections of the UKI it inherited.

`os/Containerfile.test` and `os/Containerfile.installer` both do
`FROM localhost/tilo:latest`, so both have to be rebuilt from the unsealed rootfs
(`podman build --target rootfs`) and sealed themselves. Until they are, their
container builds still succeed and the *disk* step fails at the digest check with
the message `os/dev/disk-image.sh` prints — a legible failure rather than an image
that boots wrong, but outstanding work either way.

**bootupd had to go.** A sealed image may not offer its install path, and this was
listed above as a real conflict because `tilo-secureboot` was built around
`bootupctl update` rewriting the ESP. It turned out not to be one: with a UKI there
is exactly one PE binary on the boot path, `tilo-crypto sign-efi` over it is the whole job, and
`tilo-secureboot-sign.path` already watched `/boot/EFI/Linux`.

**`bootc container lint` survives a split-kernel UKI image.** It exits 0 against
a built one under bootc 1.16.4 (4 warnings, none of them about the kernel or
/boot), so it does not have to move ahead of the kernel split. The `kernel` fatal
check counts `/usr/lib/modules/$kver` directories and the split leaves the
directory in place; `nonempty-boot` is a warning, not a fatal.
