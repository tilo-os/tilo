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
python3 tools/perf/image_size.py --dev-base registry.tilo.so/tilo/tilo-vm:x86_64-ci \
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
  `t dispatch perf-publish --publish --ref main --input accept=<KPI ids>` once the image has it.
- **Ceiling.** Compressed bytes and unique content are held to an enforced
  ceiling, set 5 % over the image #4003 measured and lowered by what each
  later size change saved, so slow creep fails too. It may only be lowered,
  except by a change that says why in the same pull request; a change that
  shrinks the image lowers it by its measured saving and records the
  numbers under `docs/benchmarks/`. The KPI targets (3.5 GB compressed, 7 GB of content for
  x86_64) are written beside the ceilings. Windows compatibility is most of
  the gap; shipping it as a downloaded system extension would close most of it.

## Layers an update sends

An update downloads every layer whose digest the device does not have. The
image is composed from layer actions (`os/image/layers.toml`,
`os/image/build-layer.sh`): each builds its piece's Containerfile stages over
the layers below and keeps the layers it added (`tools/ci/os/oci_layers.py
take`). Every action builds with one `SOURCE_DATE_EPOCH`, so two builds whose
inputs to a layer are the same make the same layer, and an update sends only
the layers whose content changed.

The preinstalled apps' trees (`/usr/share/tilo-apps/trees/<payload root
hash>`) arrive in one COPY, which made one layer of every app: a one-constant
change in one app re-sent all of them, about 50 MB. `take` gives each
directory below an `APART` path a layer of its own (with the entries' bytes
as the COPY wrote them; a hard link to another tree's file becomes a copy of
it), so an update sends only the apps that changed. That is one layer per
app, about 65 more; the image stays far below the 500 lower layers overlayfs
(and so containers-storage) stacks, and bootc's composefs backend, which
devices update with, stores layers without stacking them.

`--update-from OLD` measures an update: the compressed layers the image has
and OLD does not. `--update-only` reads only the two manifests:

```bash
python3 tools/perf/image_size.py --image registry.tilo.so/tilo/tilo-ci:main-<new>-x86_64 \
  --update-from registry.tilo.so/tilo/tilo-ci:main-<old>-x86_64 --update-only
```

Every lane builds with one `SOURCE_DATE_EPOCH`, the newest creation time of
the base and the RPM snapshot (`os/dev/image-epoch.sh`), not the commit time.
Content embeds it: dracut writes it as every cpio time of the initramfs, so
the 116 MB UKI was a new blob in every release-lane build although its files
were the same; dnf records it as each package's install time in the rpmdb;
`fc-cache` records the font directories' times. With it stable, two release
builds share those layers unless what is in them changed. A publication still
reseals a second UKI on top, which carries the new tree's composefs digest and
is new in every release by design. The release lane no longer re-sends the UKI (116 MB) and the rpmdb (17 MB) when
nothing in them changed (`docs/benchmarks/os-image-size-2026-10-12-update-layers.md`).

The nightly footprint group records `footprint.image.update_bytes.<arch>`,
the update from main's previous build (`image_size.py --update-from-previous
'main-{sha7}-{arch}'`), under an enforced ceiling in
`tools/perf/kpi-budgets.toml`.
