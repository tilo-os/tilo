# Native libvirt packages

ARM64 Windows appliances use libvirt's protected UEFI variable-store support.
The current Fedora snapshot's libvirt 12.0 lacks it. The maintained input is
libvirt 12.7.0, built with the spec included in its upstream release archive,
with upstream tests enabled on the native architecture.

`os/Containerfile.libvirt` has two build products:

- `libvirt-buildroot`: Fedora dependencies resolved against `BASE_REF`, including
  the English locale required by the upstream table-formatting tests. Preserve
  and pin this native image by digest before using it for release builds.
- The default target: a `/repo` containing RPMs, repository metadata, upstream
  spec, source checksum and package inventory. With a prepared buildroot this
  target builds without network access.

Materialize the source with:

```sh
bash os/dev/cache-github-assets.sh libvirt-12.7.0.tar.xz
```

Pass that cache directory as the `github-assets` named context and `BASE_REF`
from `os/base-snapshot.env`. Build `--target libvirt-buildroot` first. Then build
the default target with `LIBVIRT_BUILDROOT_REF` set to that image's immutable
reference and networking disabled. `BUILD_JOBS` defaults to two and must be a
positive integer. Use a native x86-64 or ARM64 builder for each output; tests
run as part of `rpmbuild`, not through cross-architecture emulation.

The source archive's SHA-256 is in the shared external asset manifest. Its
detached signature was checked against the upstream HTTPS key. No independent key certification is claimed.

Do not treat a local development tag as an approved release input. The packages
still need inclusion in the frozen RPM snapshot, signed publication, and full
OS and Windows guest acceptance. The initial local native build passed 161
upstream tests with one expected failure and no skips. The x86-64 recipe path
has not yet been executed locally. Byte-for-byte reproducibility across separate
RPM builds has not been established.
