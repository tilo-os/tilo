Pinned NTFS-3G source, not a system ABI fallback
=============================================

`ntfs-3g-source.tar.gz` is the unmodified GitHub codeload archive for
`d327833ec1d5eb1358b6f2c37139f10a3460944d`. Its SHA-256 is
`b90caed8fade648f119c767bd310419be56a2b0014b16e6b2c68b50bc6c7e9cd`
and its length is 905743 bytes. `ntfs-3g.lock` records the original URL.
`build_native.py` checks the entire archive before extraction. It never downloads
anything, and requires both expected parser symbols and absence of the default
Unix/Windows device adapter in the produced archive.

The upstream archive contains its source copyright/license notices. COPYING is
also copied here for redistribution. NTFS-3G and this separately linked helper
are GPL-2.0-or-later. Preserve the source archive, lock, notices and the helper
source/build instructions with distribution; this is not a replacement of the
host's NTFS packages. No upstream source patch is applied.

Bootstrap prerequisites: C compiler, make, Python 3, autoconf, automake,
libtool, pkg-config macros, and libgcrypt development m4 macros. Crypto is still
explicitly disabled; the bootstrap requires the macro even with that flag.
Local verification used autoconf 2.73, automake 1.19, libtool 2.6.2 and libgcrypt
1.12.4 m4 macros. The separately configured fixture-tools library includes default
I/O and mutating utilities. It is never linked into the production reader.
