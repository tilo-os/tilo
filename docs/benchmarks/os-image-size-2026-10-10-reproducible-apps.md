# OS update size, 2026-10-10: reproducible app packages

A follow-up of [the rechunk receipt](os-image-size-2026-10-10-rechunk.md).
After content-based layering an update that changed one app downloaded 215 MB, and an
update that changed nothing the image reads cost the same.

## What differed between two builds

Every tar member of the 7 layers that differed between that receipt's two images
(`8e6975576c` and the same commit with one app changed), hashed: every
thin-app binary, both runtime bundles' payloads and every tar archive came
out byte-identical. The image build signed its ~60 apps with a key minted per
build. The key is in each package's signed root, whose BLAKE3 hash is the
package id and names the payload directory, so 161 MB of identical binaries
moved to new `/usr/share/tilo-apps/pkgs/<id>/` paths and all 59 archives in
`/usr/share/tilo-apps/cohort/` (163 MB) had new bytes at the same size. The
rest was small: the runtime bundles' `bundle.toml` and publisher keys
(their keys are minted per build on purpose: the launcher trusts them),
`registry.toml` and systemd's `/etc/.updated` stamp, each of which renewed
the 40 MB layer of Tilo's small files.

## Method

Build / OS dev dispatches (x86_64) on throwaway branches, each a commit on
top of the change: a docs-only commit, a second docs-only commit, then
`apps/calculator` `HISTORY_CAP` 64 → 65, as the rechunk receipt measured. A docs commit
changes the image's build sources, so every build step runs again (through
the compiler cache), as on any real commit. Updates are
`python3 tools/perf/image_size.py --image NEW --update-from OLD --update-only`:
the compressed layers of the new manifest the old one lacks. Raw output in
[`os-image-size-2026-10-10-reproducible-apps.json`](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/benchmarks/os-image-size-2026-10-10-reproducible-apps.json).

Round 1 is the first commit of the change (one app identity for dev images,
`DEV_APP_PUBLISHER_IDENTITY`; the runtime signatures in a layer of their
own). Round 2 adds the app store's own chunkah run (up to 32 layers), and
puts its index (`registry.toml`, `pkgs/*.pkg/*`) and `/etc/.updated` into
small layers of their own.

## Update size (x86_64)

| update | rechunk receipt | round 1 | round 2 |
|---|---:|---:|---:|
| nothing the image reads changed | 215,088,007 B (7 of 145 layers) | 40,817,567 B (3 of 146) | **264,448 B** (2 of 179) |
| one app changed | 215,178,773 B (7 of 145) | 116,641,392 B (5 of 146) | **33,330,775 B** (6 of 179) |

Runs: round 1 94378 → 94379 → 94380 (`f7d207d`, `be9db28`, `9d89a67`);
round 2 94780 → 94781 → 94782 (`60b38df`, `22e29bd`, `1734abf`). The full
image stays 4,255.9 MB compressed.

What round 2's updates carry:

- nothing changed: the build-signatures layer (17 KB: both runtime
  `bundle.toml`s, three publisher keys, the runtime trust list,
  `/etc/.updated`) and the rechunk's directory layer (0.25 MB).
- one app: those two, the app-store index layer (0.2 MB), the two app-store
  layers holding Calculator's binary and archive (13.6 MB and 14.5 MB, ~35
  files each) and the app store's small-files layer (4.8 MB, which carries
  the new payload directory's entries).

Round 1, file by file: all 59 unchanged archives and payload trees were
identical between builds; the one-app pair differed only in Calculator's
binary, archive and metadata, `registry.toml`, and the per-build files above.

## Reproducibility check

Release / apps now packages Calculator twice (`tools/apps/check-reproducible-tapp.sh`),
the second time after removing its build outputs and compiling it past the
compiler cache, and compares every output byte for byte. Dispatch 94381:
aarch64 231 paths identical (57 s), x86_64 226 paths identical (87 s).

## Not covered

- Two compiles on different runners without any compiler cache.
- Dev images that take the published app collection (release-signed,
  compiled by Release / apps) and those that compile the apps in the image
  share no app bytes; switching between the two re-sends every app.
- Release images, which take the published collection; Release / OS has
  not run on main yet.
