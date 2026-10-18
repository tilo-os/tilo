# The files of this tree that are not compiled: scripts, units, keys. Its
# compiled files are a layer of their own (binaries.Containerfile), so a changed
# binary doesn't change the system layer these go into.
FROM scratch AS recovery-files
COPY --chmod=0755 \
    os/usr/libexec/tilo-deployment \
    os/usr/libexec/tilo-system-extension-activate \
    os/usr/libexec/tilo-system-extension-dispatch \
    os/usr/libexec/tilo-boot-health \
    os/usr/libexec/tilo-esp-mount \
    os/usr/libexec/tilo-repair \
    os/usr/libexec/tilo-recovery \
    os/usr/libexec/tilo-update-policy-set \
    os/usr/libexec/tilo-update-policy-check \
    /usr/libexec/
COPY --chmod=0755 os/usr/lib/systemd/system-generators/tilo-boot-mode-generator /usr/lib/systemd/system-generators/tilo-boot-mode-generator
COPY --chmod=0755 os/usr/lib/systemd/system-generators/tilo-esp-generator /usr/lib/systemd/system-generators/tilo-esp-generator
COPY \
    os/usr/lib/systemd/system/tilo-boot-entries.service \
    os/usr/lib/systemd/system/tilo-boot-entries.timer \
    os/usr/lib/systemd/system/tilo-esp-write@.service \
    os/usr/lib/systemd/system/tilo-boot-health.service \
    os/usr/lib/systemd/system/tilo-repair.target \
    os/usr/lib/systemd/system/tilo-repair@.service \
    os/usr/lib/systemd/system/tilo-deployment-rollback.service \
    os/usr/lib/systemd/system/tilo-deployment-update.service \
    os/usr/lib/systemd/system/tilo-system-extension-userspace.service \
    os/usr/lib/systemd/system/tilo-system-extension-boot-stage.service \
    os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.service \
    os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.timer \
    os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.path \
    os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service \
    os/usr/lib/systemd/system/tilo-local-model.service \
    os/usr/lib/systemd/system/tilo-system-extension-dispatcher.socket \
    os/usr/lib/systemd/system/tilo-system-extension-dispatcher@.service \
    /usr/lib/systemd/system/
COPY \
    os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/10-tilo-boot-entries.conf \
    os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/20-tilo-update-policy.conf \
    os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/30-tilo-background-resources.conf \
    /usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/
COPY os/usr/lib/systemd/system/systemd-bless-boot.service.d/10-tilo-writable-esp.conf \
    /usr/lib/systemd/system/systemd-bless-boot.service.d/10-tilo-writable-esp.conf
COPY os/usr/lib/systemd/system/tilo-update-policy@.service /usr/lib/systemd/system/tilo-update-policy@.service
COPY os/etc/polkit-1/rules.d/49-tilo-updates.rules /etc/polkit-1/rules.d/49-tilo-updates.rules
COPY os/usr/share/tilo/system-extensions /usr/share/tilo/system-extensions
COPY \
    os/usr/share/doc/tilo/system-extensions.md \
    docs/recovery.md \
    docs/security/repair-mode.md \
    /usr/share/doc/tilo/
COPY --chmod=0755 os/tests/recovery-boot-check.sh /usr/libexec/tilo-recovery-boot-check
