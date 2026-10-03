# Development WPE stage cache

The optional `TILO_WPE_BUILD_CACHE_REF` setting enables only the native WPE
engine stage cache during fresh development image builds. It must name an
untagged repository under `registry.tilo.so/tilo/tilo-development-wpe-cache-`
ending in the native architecture (`x86_64` or `aarch64`). Registry write access
is a build trust boundary: writers can supply cached build output. Keep this
repository restricted to authorized development CI writers.

`TILO_WPE_SOURCE_DATE_EPOCH` is required explicitly. A stable value such as `0`
is the dependency artifact epoch, independent of the OS release epoch. The
helper passes it as both the compiler build argument and Podman layer timestamp
policy. This provides stable cache inputs and timestamps; it does not establish
bit-for-bit reproducible compiler output.

`prepare-wpe-stage.py` builds only `wpe-builder-1` with Buildah-compatible
`--layers --cache-from --cache-to`, then validates its immutable local image ID,
Linux OS and native architecture. The full build uses that ID to override the
`wpe-builder` context. Isolated `os/Containerfile.wpe` recipe, pinned buildroot and mounted frozen assets
remain cache inputs. The full OS retains its own source epoch, public trust,
generation and revision. Raw and signed rootfs layers are never exported by
this cache path. BuildKit cache manifests are not interchangeable with these
Buildah intermediate-image repositories.

The cache preserves completed WPE output. Compiler cache mounts are not exported
as image layers, so this does not resume a failed partial WebKit compilation.
The existing full browser and application payload remain required.

## Native acceptance before activation

The setting is opt-in for the general image builder. The provisional compile
driver enables it after native acceptance in
[run 6992](https://code.tilo.so/tilo/tilo/actions/runs/6992), source
`d161be91dffe82cec90a4ab3d9be3b1b3eb525eb`: both x86-64 and ARM64 passed with
Podman 5.8.4. This small disposable fixture uses a local registry and
two separate VFS stores. It requires a cold build followed by a remote cache
hit in the fresh store without executing the build script. It also checks
recipe, frozen asset and epoch invalidation, and immutable stage reuse by an OS
consumer with a different release epoch and public marker. It does not compile
WebKit or establish full OS capacity or boot readiness.

## Import a completed WPE artifact

Set `TILO_WPE_STAGE_RECEIPT` to the public receipt produced by the dedicated
WPE stage job and set `TILO_WPE_SOURCE_DATE_EPOCH` to that dependency epoch.
For fresh full-browser development builds, `build-image.sh` calls
`wpe-stage-artifact.py import` with the current native architecture, checkout,
frozen assets and pinned WPE buildroot. The importer validates dependency inputs
and registry/payload identity before returning a full immutable local image ID.
The OS build uses that ID as its WPE stage context while retaining its own
release epoch and complete application build.

An explicit receipt takes precedence over `TILO_WPE_BUILD_CACHE_REF`. An invalid,
incompatible or unavailable artifact aborts the build; it never falls back to
compiling WPE. Without a receipt, existing cache and default build behavior are
unchanged. The receipt must come from the reviewed development producer; it is
not a production signature or installation authority.

## Adopt a reviewed retained development build

An older local engine may have usable compiled bytes without the current CI
producer receipt. `adopt-development-wpe.py` handles that separate case. A
reviewed, independently pinned audit binds the original engine and buildroot
identities, old recipe, source archives, and compatible current consumer recipes.
The helper verifies the native payload, projects only `/wpe` without executing
image code, and publishes it to a dedicated development adoption repository.
Its receipt preserves unknown original build epoch and source revision; the
projection timestamp describes only the new packaging layer.

For an independently reviewed adoption receipt, set
`TILO_WPE_ADOPTION_RECEIPT` and `TILO_WPE_ADOPTION_SHA256`. The image consumer checks
the receipt, embedded audit, current recipes and source archives, then verifies
the immutable registry image and complete payload inventory. It imports the
existing engine through the same named build context. The unused WPE compiler
buildroot is not required. Adoption cannot be combined with another WPE source,
production trust, or release buildroot-signature verification. Failure stops
before the OS build; it never triggers a replacement WebKit compilation.

This is development provenance and reuse, not a reproducible rebuild claim,
production signature, runtime ABI test, or boot proof.
