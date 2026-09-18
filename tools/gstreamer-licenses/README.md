# Shared Windows media runtime notices

The bundle's `gstreamer-runtime.json` records every shipped executable/DLL,
SHA-256 digest, originating MSYS2 package, exact installed package version,
declared license, upstream project URL and system DLL imports. Package-provided
license files are copied under `packages/` without changing their contents.

MSYS2 packages do not consistently include standard license texts. The
`supplemental/` directory supplies the GPL 2/3, LGPL 2/2.1/3, Apache 2.0, ISC,
BSD-3-Clause and WTFPL texts from SPDX license-list-data v3.27.0. It also supplies
version-specific upstream copyright notices omitted by the libass, libtheora,
libvorbis, libvpx, p11-kit and libx264 packages. `supplemental/sources.json`
records their exact source URLs and SHA-256 digests; the bundler verifies them.

This MSYS2 FFmpeg build declares GPL-3.0-or-later, and the dependency closure
contains GPL libraries. It must not be described as an LGPL-only distribution.
The installer assembles packages already installed on the user's own machine;
it does not provide a third-party binary download. Distributing this assembled
runtime requires the corresponding source and applicable redistribution
materials, beyond this local installation script and these license notices.

MSYS2 package build recipes and patches are available at
https://github.com/msys2/MINGW-packages. Consult each recorded upstream project
and exact package version when preparing corresponding source for distribution.
