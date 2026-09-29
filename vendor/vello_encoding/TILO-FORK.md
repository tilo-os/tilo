# tilo-vello-encoding

Tilo's fork of the crates.io `vello_encoding` 0.9.0, published as `tilo-vello-encoding` so the Tilo
SDK can be published with it. The library is still named `vello_encoding`, so code
keeps writing `vello_encoding::…`. Upstream: https://github.com/linebender/vello
(Apache-2.0 OR MIT, unchanged).

What differs from the upstream release is listed in the Tilo repository's
`vendor/PATCHES.md` ("Vello changes"): scratch buffers are sized from what a
frame measurably uses instead of fixed worst-case constants. The changes are
proposed upstream as linebender/vello#1861, #1862 and #1863; when a vello
release carries them, the Tilo SDK moves back to `vello_encoding` and this fork ends.
