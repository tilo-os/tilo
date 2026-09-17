# Development WPE stage cache

The optional `TILO_WPE_BUILD_CACHE_REF` setting enables only the native WPE
engine stage cache during fresh development image builds. It must name an
untagged repository under `code.sandbox.example/qos/tilo-development-wpe-cache-`
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
`wpe-builder` context. Source recipe, pinned buildroot and mounted frozen assets
remain cache inputs. The full OS retains its own source epoch, public trust,
generation and revision. Raw and signed rootfs layers are never exported by
this cache path. BuildKit cache manifests are not interchangeable with these
Buildah intermediate-image repositories.

The cache preserves completed WPE output. Compiler cache mounts are not exported
as image layers, so this does not resume a failed partial WebKit compilation.
The existing full browser and application payload remain required.

## Native acceptance before activation

The setting is disabled by default. The provisional compile driver must not
activate it until `development-source.yml` passes `wpe_stage_cache.py` on both
native architectures. This small disposable fixture uses a local registry and
two separate VFS stores. It requires a cold build followed by a remote cache
hit in the fresh store without executing the build script. It also checks
recipe, frozen asset and epoch invalidation, and immutable stage reuse by an OS
consumer with a different release epoch and public marker. It does not compile
WebKit or establish full OS capacity or boot readiness.
