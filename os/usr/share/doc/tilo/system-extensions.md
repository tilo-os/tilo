# System extensions

See `/usr/share/tilo/system-extensions/README.md` for the versioned system
extension contract. Tilo intentionally distinguishes userspace local-model
runtimes, which may be started without a reboot, from kernel modules and GPU
driver stacks, which must be included in a new immutable deployment and rebooted
with normal rollback protection.

Driver-pack requests use only an image-owned signed catalog id. Tilo stages the
corresponding digest-pinned bootc deployment and exposes reboot-required state;
it never installs or hot-loads a driver into the running system.
