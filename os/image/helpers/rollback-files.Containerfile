# The files of this tree that are not compiled: scripts, units, keys. Its
# compiled files are a layer of their own (binaries.Containerfile), so a changed
# binary doesn't change the system layer these go into.
FROM scratch AS rollback-files
COPY \
    os/usr/libexec/tilo-rollback-common \
    os/usr/libexec/tilo-rollback-confirm \
    os/usr/libexec/tilo-rollback-harden \
    os/usr/libexec/tilo-rollback-initrd-unlock \
    os/usr/libexec/tilo-root-discover \
    /usr/libexec/
COPY \
    os/usr/lib/systemd/system-generators/tilo-rollback-unlock-generator \
    os/usr/lib/systemd/system-generators/tilo-root-generator \
    /usr/lib/systemd/system-generators/
COPY os/usr/lib/dracut/modules.d/91tilo-rollback /usr/lib/dracut/modules.d/91tilo-rollback
COPY os/usr/lib/dracut/modules.d/92tilo-sibling-unlock /usr/lib/dracut/modules.d/92tilo-sibling-unlock
COPY \
    os/usr/lib/systemd/system/tilo-rollback-confirm.service \
    os/usr/lib/systemd/system/tilo-rollback-confirm.timer \
    os/usr/lib/systemd/system/tilo-rollback-harden.service \
    os/usr/lib/systemd/system/tilo-rollback-harden.timer \
    /usr/lib/systemd/system/
COPY os/tests/rollback-boot-check.sh /usr/libexec/tilo-rollback-boot-check
COPY os/tests/boot_units_gate.sh /usr/libexec/tilo/boot-units-gate.sh
