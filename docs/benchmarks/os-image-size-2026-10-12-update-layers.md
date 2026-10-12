# OS update size, 2026-10-12: one epoch, and layer trees for what changes on its own

A follow-up of [the reproducible-apps receipt](os-image-size-2026-10-10-reproducible-apps.md).
Between two consecutive release-lane builds (`fa2c303` → `4e5169e`, Release / OS
validation runs 98406 and 98950, about 40 commits apart) an update downloaded
**699,677,229 B on x86_64** (46 of 179 layers, 16.5 %) and **641,638,762 B on
aarch64** (45 of 179, 19.0 %). Read layer by layer, about a third of it was bytes
whose content had not changed.

## What did not change but was re-sent

- **The UKI (115,937,888 B x86_64, 101,340,489 B aarch64), in every release-lane
  build.** All PE sections were identical except `.initrd`. Its 3,600 + 7 cpio
  entries were identical except their mtime, 1791718273 → 1791729105, which
  are the two commit times. The release lane built with
  `SOURCE_DATE_EPOCH` = the commit time, and dracut writes it into the archive.
- **The rpmdb (16,626,316 B x86_64, 14,823,754 B aarch64).** Same packages in
  the same order; header tags 1008 and 1128 (install time and transaction id)
  of packages 509–1342 differed. dnf's build clock is the epoch.
- **The fontconfig caches**, which record the font directories' times, pinned
  to the epoch.
- **The native runtime's unchanged files.** Its directory is
  `/usr/lib/tilo-runtime/<release>+native.<payload digest>`, so one changed
  library renames every file in it. Its fonts, libc, glib, gstreamer and libstd
  sat in chunkah's main run beside Chromium's `.pak` files (two mixed layers,
  54,848,759 B) and the image-wide unclaimed small files (35,880,272 B). Every
  rename re-sent all three layers.
- **31 RPMs (20,912,818 B).** A new Tilo binary (`libtilo_spotify_engine.so`)
  changed chunkah's size-outlier cap, and one RPM moved into their hash bin.

## The change

- Every lane builds with the newest creation time of the base image and the RPM
  snapshot (`os/dev/image-epoch.sh`). The dev lane already did, and its UKI and
  rpmdb layers were the same blob in all four builds below. The commit time
  stays in `org.opencontainers.image.created`.
- `os/scripts/rechunk-chunkah.py` gives these chunkah runs of their own:
  - the native runtime, together with its fonts' hard-linked names in
    `/usr/share/fonts/tilo` and the font catalogues;
  - the Chromium runtime;
  - Tilo's programs and data, with a budget above their ~40 components so that
    nothing in them is packed.

  The main run keeps the RPMs, with a budget of 96, and prunes each tree whole,
  directory included.

## Method

Build / OS dev dispatches (x86_64, the dev lane; the rechunk is the same as the
release lane's) on throwaway branches:

| build | commit | run |
|---|---|---|
| before-a | `f17f0b0`, main | 99516 |
| before-b | before-a plus one comment line at the top of `crates/sdk/ui-core/src/app.rs` | 99886 |
| after-a | `375ea8b`, this change on `f17f0b0` | 100678 |
| after-b | after-a plus the same comment line | 101099 |

The comment line shifts every panic location in the file. That changes the
runtime library (and so renames the runtime), the shell, the greeter, the
installer and every app. It is the most common kind of update: a framework
change. Updates are `tools/perf/image_size.py --image NEW --update-from OLD --update-only`.
The raw output is in `os-image-size-2026-10-12-update-layers.json`.

## Update size (x86_64, dev lane)

| update | layers new | bytes |
|---|---:|---:|
| before: framework change, old layering | 42 of 179 | 518,320,716 |
| **after: framework change, new layering** | 38 of 197 | **448,067,588** (−70,253,128, −13.6 %) |
| first round, runtime directory still in a main-run bin | 39 of 196 | 504,845,701 |
| one-time move to the new layering, same code | 53 of 197 | 475,395,864 |

The full image goes from 4,258,473,003 to 4,260,769,276 B (+0.05 %), because
nine more hard-link groups are split across layers. It has 197 layers, well
under containers-storage's 500. The rechunked-vs-raw tree proof passed on
every build (109,802 paths equal).

What the after update carries:

- **The native runtime, 140.7 MB in five layers.** Every file in it is renamed.
  Before, the runtime's share was 210.9 MB, because its bigfiles, the two
  layers mixed with Chromium's files, and the image-wide unclaimed layer all
  changed.
- **shell, greeter and installer, 173.9 MB.**
- **The apps, 124.1 MB.** Each app ships twice, as its binary and as its cohort
  `.tapp`.
- **The app store's small files and index, 9.2 MB**, and the directory layer,
  0.25 MB.

Layers that no longer change: Chromium's (54.8 MB of mixed layers before), the
main run's unclaimed small files and its RPM bins, and the UKI.

## Release lane

The epoch change removes the UKI and the rpmdb from every release-lane update
whose inputs to them did not change: −132,564,204 B on x86_64 and
−116,164,243 B on aarch64 for the pair above. Together with the trees (about
−70 MB for the runtime and Chromium, and −21 MB for the RPM bin shift), that
pair would send about 476 MB instead of 700 MB on x86_64.

The estimate is not measured on the release lane. That needs two Release / OS
dispatches, and a publication is an operator step.

A publication still reseals a second UKI on top, after the rechunk. It carries
the new tree's composefs digest and is new in every release by design.

## Regression budget

The nightly footprint group records `footprint.image.update_bytes.<arch>`: the
update from main's previous build (`image_size.py --update-from-previous
'main-{sha7}-{arch}'`). The enforced ceiling is 900 MB per architecture
(`tools/perf/kpi-budgets.toml`). Consecutive rechunked main builds sent
420–830 MB before this change.

## Left

- The runtime rename itself: 141 MB per framework change, of which about 40 MB
  is unchanged (fonts, libc, glib, libstd). Fixing it means content-addressing
  the runtime's stable parts.
- The resealed UKI: 116 MB per release. Its initramfs carries
  `tilo-sibling-unlock` (28.6 MB) and `tilo-rollback` with their symbol
  tables. The placeholder UKI stays in the image under the reseal layer.
- Apps twice, binary and cohort archive: half of the 124 MB.
