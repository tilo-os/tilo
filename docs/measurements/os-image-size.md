# OS image size

Every install downloads the OS image and every update pulls its changed
layers, so its size is a KPI too. `tools/perf/image_size.py`
measures one image without a container engine: it streams each layer from
the registry, checks it against its digest and replays the layers with their
whiteouts. It reports, in bytes:

- `compressed`: the layer blobs a full pull downloads;
- `unique_content`: the merged root's regular files with identical files
  counted once, which is what composefs stores after install. Files that only
  `/sysroot` names are left out and reported as `undeployed`: the base
  image's own ostree repository, hard-linked to its `/usr`, which bootc does
  not deploy. Removing a base package from `/usr` therefore shrinks the
  installed root but not the pull;
- `redundant_payload`: bytes the layers carry beyond one copy of each distinct
  file (the same file in two layers or at two paths, or one a later layer
  replaced). composefs dedupes these on disk, but every pull still downloads
  them.

It also breaks the image down by layer (with the instruction that made it),
by directory, by file and by installed package (from the image's own rpmdb).
`--why PACKAGE` prints the chain of requires and recommends that pulls a
package in:

```bash
python3 tools/perf/image_size.py --dev-base registry.tilo.so/tilo/tilo-vm:main-ci \
  --arch x86_64 --markdown image.md --json image.json --why python3-botocore
python3 tools/perf/image_size.py --image registry.tilo.so/tilo/tilo-ci:<ref>-<sha7>-x86_64 --markdown image.md
```

`--dev-base` measures the production-layout `tilo-ci` image that a dev image
names as its base. Pull requests that touch the image publish
`tilo-ci:<ref>-<sha7>-<arch>` (Build / OS dev), so measure a branch with
`--image` before merging. t's `perf-footprint-image` check, in the performance run,
measures main's newest image for both architectures and records
`footprint.image.{compressed_bytes,unique_content_bytes,redundant_payload_bytes}.<arch>`.
`tools/perf/kpi-budgets.toml` holds them to two rules:

- **Growth.** More than 5 % above the median of the last five nights fails on
  the second run in a row. To land intended growth, run
  `t dispatch perf-scheduled --ref main --input accept=<KPI ids>` once the image has it.
- **Ceiling.** Compressed bytes and unique content are held to an enforced
  ceiling, set 5 % over the image #4003 measured and lowered by what each
  later size change saved, so slow creep fails too. It may only be lowered,
  except by a change that says why in the same pull request; a change that
  shrinks the image lowers it by its measured saving and records the
  numbers under `docs/benchmarks/`. The KPI targets (3.5 GB compressed, 7 GB of content for
  x86_64) are written beside the ceilings. Windows compatibility is most of
  the gap; shipping it as a downloaded system extension would close most of it.

## Content-based layers (rechunking)

An update downloads every layer whose digest the device does not have. A
Containerfile layer is a new blob whenever its instruction runs again, so
with one layer per instruction an update between two commits that changed one
app pulled about 78 % of the image. `os/dev/rechunk-image.sh` re-lays an image
out by content; it needs a BuildKit builder, so the releases t publishes
(`publish-os-release`) are not rechunked yet:

- `os/Containerfile.rechunk` runs [chunkah](https://github.com/coreos/chunkah)
  (built offline from its checksum-pinned release) over the raw image's root:
  one component per RPM package, read from the image's rpmdb, files over 1 MB
  on their own, the rest grouped, packed into at most 96 layers. An unchanged
  component is the same blob in the next build. Whited-out bytes, the base's
  old kernel and `/sysroot`'s ostree objects are in no layer. Trees that change
  on their own schedule get a chunkah run and layers of their own
  (`os/scripts/rechunk-chunkah.py`): Proton and the Steam runtime; the
  preinstalled apps, whose binaries and archives change one app at a time; the
  native runtime, whose directory is named by its payload's digest so that one
  changed library renames every file in it (with its fonts' hard-linked names
  in `/usr/share/fonts/tilo` and the font catalogues); the Chromium runtime;
  and Tilo's own programs and data (`/usr/bin/{shell,fs,app,tilo-*}`,
  `/usr/libexec/tilo*`, `/usr/lib/tilo`, `/usr/share/tilo`, the UKI), with
  more layers than components so that chunkah packs nothing there. Otherwise a
  runtime rename re-sends every layer its files share with unchanged ones, and
  one new Tilo binary moves RPMs between the main run's bins: which components
  share a layer depends on every component in the run. The main run is left
  with the RPMs and the few other files no package owns. Small files that are new in every build although
  nothing they describe changed (the runtime bundles' signatures and
  publisher keys, whose keys are minted per build because the launcher trusts
  them; systemd's update stamp), and the app store's index, which changes
  with any app, each get one small layer of their own for the same reason.
- `os/scripts/oci-rechunk.py` takes extended attributes from the raw image's
  layers where chunkah's differ (BuildKit's snapshot of the image, which
  chunkah reads, lacks the base binaries' file capabilities), adds one layer
  with every directory's exact metadata (chunkah caps a directory's time per
  layer; fontconfig's cache needs font directories' times unchanged), gives
  the result the raw image's config,
  and refuses it unless every path matches the raw image: type, mode, owner,
  xattrs, bytes, symlink target. Only file times may be lower (capped at the
  package's build time, or for files no package owns at an epoch that moves
  with the base and the RPM snapshot only), `/sysroot`'s contents are gone,
  and a hard-link group may be split across layers.
- The dev lane pushes the rechunked image as `tilo-ci:<ref>-<sha7>-<arch>`
  and the raw one beside it as `…-<arch>-layered`; a release rechunks its
  archive, and its independent rebuild's, before the install gate seals it.
  The repository variable `TILO_RECHUNK=0` turns it off without a commit.

Local images (`os/dev/build-image.sh`, `tilo device vm upgrade --from local`) keep
their Containerfile layers: podman's layer cache is what makes those rebuilds
cheap, and tilo-vm sends the VM only the blobs it lacks either way.

`--update-from OLD` measures an update: the compressed layers the image has
and OLD does not. `--update-only` reads only the two manifests:

```bash
python3 tools/perf/image_size.py --image registry.tilo.so/tilo/tilo-ci:main-<new>-x86_64 \
  --update-from registry.tilo.so/tilo/tilo-ci:main-<old>-x86_64 --update-only
```

An update between two commits that change one app downloaded 215 MB of the
rechunked x86_64 image (5 %), where the layered image re-sent 3.69 GB (78 %)
(`docs/benchmarks/os-image-size-2026-10-10-rechunk.md`). Most of the 215 MB
was the ~60 preinstalled apps, byte-identical but signed by a key minted per
build: the key is in each package's signed root, whose hash is the package id
and names the payload directory, so every binary moved and every cohort
archive changed. Build / OS dev now signs the apps it compiles with
one identity, the repository secret `DEV_APP_PUBLISHER_IDENTITY`, passed to the
image build as the BuildKit secret `app_publisher_identity`. It is in no trust
list (`os/Containerfile`, the app store's COPY) and never reaches a
publication, which takes the released collection. Without it (local builds)
the image mints one as before. With it, an update between two builds that
change nothing the image reads is 0.26 MB, and one that changes one app
33 MB (`docs/benchmarks/os-image-size-2026-10-10-reproducible-apps.md`).

Every lane builds with one `SOURCE_DATE_EPOCH`, the newest creation time of
the base and the RPM snapshot (`os/dev/image-epoch.sh`), not the commit time.
Content embeds it: dracut writes it as every cpio time of the initramfs, so
the 116 MB UKI was a new blob in every release-lane build although its files
were the same; dnf records it as each package's install time in the rpmdb;
`fc-cache` records the font directories' times. With it stable, two release
builds share those layers unless what is in them changed. A publication still
reseals a second UKI on top, which carries the new tree's composefs digest and
is new in every release by design. With the runtime, Chromium and Tilo trees,
an update for a framework change went from 518 MB to 448 MB on x86_64, and the
release lane no longer re-sends the UKI (116 MB) and the rpmdb (17 MB) when
nothing in them changed (`docs/benchmarks/os-image-size-2026-10-12-update-layers.md`).

The nightly footprint group records `footprint.image.update_bytes.<arch>`,
the update from main's previous build (`image_size.py --update-from-previous
'main-{sha7}-{arch}'`), under an enforced ceiling in
`tools/perf/kpi-budgets.toml`.
