## Runtime-local emptyDirs

Kata runtime-rs LocalStorage (source revision
337b6002681479fb6a605ca8a7a1138e81b6098c,
`src/runtime-rs/crates/resource/src/volume/local_volume.rs`) can map a disk
emptyDir into the sandbox/pause rootfs at `local/<volume-name>`. Agent
`storage/local_handler.rs` creates that directory; it is not a separate host
passthrough mount. Guest mountinfo consequently shows `/local/<volume-name>`
on a virtiofs superblock even though no separate volume export appears in host
mountinfo.

Admission requires fresh host-side directory observations in `local_volumes`.
The collector compares the rootfs/local/volume directory identity chain through
three views: the socket-bound virtiofs process root, the bound shim's shared
export, and the containerd sandbox rootfs in that shim's namespace. All device
and inode identities must match, all components must be directories, and there
must be no filesystem boundary or nested mount below local. The exact Pod UID,
volume name and OCI destination bind this evidence to a whole non-memory
emptyDir; subPath, symlink, foreign source and memory-medium substitutions fail.
Existing runtime/image/kernel pins, Pod revalidation, complete export inventory
and live QEMU no-host-block checks remain mandatory.

The recorded finalization preflight predates this collector field. It is not
accepted retroactively. Tests model its observed mount shape with explicitly
synthetic identity numbers; only a new live collection can establish actual
local-volume admission. Missing observations fail closed.
