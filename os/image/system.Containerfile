# ---- The system: configuration, units, policy over the packages -----------
# Everything of the OS that is neither a package nor compiled here: files from
# os/, the rendered splash and wallpapers, the SELinux module, the Windows
# provider, enabled and masked units. It changes with os/ and nothing compiled.
FROM kernel-installed AS system
# The build-only dnf/rpm wrappers read it.
ARG SOURCE_DATE_EPOCH=0
ARG FINALIZE_RPMDB="/usr/libexec/tilo/finalize-rpmdb"
ARG RPMS_REF
ARG TILO_WINDOWS_ARCH

COPY os/usr/share/flatpak/remotes.d/flathub.flatpakrepo /usr/share/flatpak/remotes.d/flathub.flatpakrepo

# Keep encrypted-root unlock in the same visual path as the UKI splash. The
# theme's PNGs are rendered at build time (plymouth-assets), so the source
# remains reviewable and the initramfs does not depend on a desktop image
# stack; a theme edit changes only this small layer.
COPY --from=plymouth-assets /out/tilo/ /usr/share/plymouth/themes/tilo/
# The desktop's wallpapers (os-wallpapers) and their credits.
COPY --from=os-wallpapers /out/backgrounds/tilo/ /usr/share/backgrounds/tilo/
COPY --from=os-wallpapers /out/licenses/tilo-wallpapers/ /usr/share/licenses/tilo-wallpapers/
RUN plymouth-set-default-theme tilo \
    && test "$(plymouth-set-default-theme)" = tilo
COPY \
    os/scripts/verify-runtime-crypto-packages.sh \
    os/scripts/verify-no-ui-toolkits.sh \
    os/scripts/verify-no-cisco-openh264.sh \
    os/scripts/verify-no-python-runtime.sh \
    /usr/local/bin/

# Confine the collaborative filesystem daemon in its own SELinux domain
# (tilo_fs_t) instead of unconfined_service_t. Shipped permissive so it can't
# break the working daemon; tightens to enforcing from captured AVCs (see the
# module header). The drop-in runs fs.service in the domain via SELinuxContext.
COPY os/usr/share/tilo/selinux/tilo-base.cil /usr/share/tilo/selinux/tilo-base.cil
COPY os/usr/lib/systemd/system/fs.service.d/20-selinux.conf /usr/lib/systemd/system/fs.service.d/20-selinux.conf
# `libsemanage` writes a sandbox at /etc/selinux/targeted/tmp during
# install and fails "File exists" if any prior partial state carried
# forward (a caching quirk under rootless podman that CI's docker
# daemon happens to sidestep). Clear it first.
# libsemanage builds its sandbox by hardlinking the current policy store, and a
# hardlink to a file that lives in a lower container layer fails EEXIST — which
# surfaces as "Could not copy files to sandbox /etc/selinux/targeted/tmp (File
# exists)" while that directory is demonstrably absent. Removing the stale
# sandbox, which this step already did, addresses the wrong half: the store
# itself has to exist in this layer. Copy it into place first.
#
# Add-on SELinux RPM scriptlets are deliberately non-fatal (`semodule ... || :`).
# Under the same rootless-overlay failure their policy archives are installed
# but their modules are absent from `active` (they can also be left only in
# abandoned `previous`/`tmp` stores). Reinstall and then verify every shipped
# add-on policy archive after copying up the store. Keeping this generic matters:
# optional desktop/virtualization packages can add a policy archive without also
# changing this Containerfile.
RUN cp -a /etc/selinux/targeted /var/tmp/tilo-sestore \
 && rm -rf /etc/selinux/targeted \
 && mv /var/tmp/tilo-sestore /etc/selinux/targeted \
 && rm -rf /etc/selinux/targeted/tmp \
 && find /usr/share/selinux/packages -type f \( -name '*.pp' -o -name '*.pp.bz2' \) -print \
      | sort \
      | while IFS= read -r policy; do \
          semodule -n -s targeted -X 200 -i "$policy" || exit 1; \
        done \
 && semodule -i /usr/share/tilo/selinux/tilo-base.cil \
 && semodule -s targeted -lfull > /var/tmp/tilo-active-selinux-modules \
 && find /usr/share/selinux/packages -type f \( -name '*.pp' -o -name '*.pp.bz2' \) -print \
      | sort \
      | while IFS= read -r policy; do \
          module="$(basename "$policy")"; module="${module%.bz2}"; module="${module%.pp}"; \
          awk -v module="$module" '$1 == "200" && $2 == module { found=1 } END { exit !found }' \
            /var/tmp/tilo-active-selinux-modules || exit 1; \
        done \
 && semodule -s targeted -lfull | grep -Eq '^400[[:space:]]+tilo-base[[:space:]]' \
 && rm -f /var/tmp/tilo-active-selinux-modules \
 && echo "all packaged add-on and tilo-base SELinux modules loaded"

# Tilo's own Secret Service owns org.freedesktop.secrets and the portal Secret
# backend. GNOME Keyring stays installed only so the first password login can
# read an existing login keyring once (tilo-secret-service imports it); nothing
# of it may claim the bus name or serve the portal, or two providers would
# race for libsecret's clients and one would sit locked after a fingerprint or
# automatic login.
RUN rm -f /usr/share/xdg-desktop-portal/portals/gnome-keyring.portal \
          /usr/share/dbus-1/services/org.freedesktop.impl.portal.Secret.service \
          /etc/xdg/autostart/gnome-keyring-secrets.desktop \
    && systemctl --global mask gnome-keyring-daemon.service gnome-keyring-daemon.socket
COPY os/usr/share/dbus-1/services/org.freedesktop.secrets.service \
    /usr/share/dbus-1/services/org.freedesktop.secrets.service
COPY \
    os/usr/lib/systemd/user/tilo-models.service \
    os/usr/lib/systemd/user/tilo-models.socket \
    os/usr/lib/systemd/user/tilo-model-gateway.service \
    os/usr/lib/systemd/user/tilo-model-gateway.socket \
    os/usr/lib/systemd/user/tilo-secret-service.service \
    os/usr/lib/systemd/user/tilo-app-host.socket \
    os/usr/lib/systemd/user/tilo-projector.service \
    os/usr/lib/systemd/user/tilo-projector.socket \
    os/usr/lib/systemd/user/tilo-cache-gc.service \
    os/usr/lib/systemd/user/tilo-boot-fallback-notice.service \
    os/usr/lib/systemd/user/tilo-cache-gc.timer \
    os/usr/lib/systemd/user/tilo-app-update.service \
    os/usr/lib/systemd/user/tilo-app-update.timer \
    os/usr/lib/systemd/user/tilo-public-content-refresh.service \
    os/usr/lib/systemd/user/tilo-public-content-refresh.timer \
    os/usr/lib/systemd/user/tilo-runtime-resident.service \
    os/usr/lib/systemd/user/tilo-runtime-resident.path \
    /usr/lib/systemd/user/
COPY \
    os/usr/lib/systemd/system/tilo-relay.service \
    os/usr/lib/systemd/system/tilo-fs-replica.service \
    os/usr/lib/systemd/system/tilo-relay-firstboot.service \
    os/usr/lib/systemd/system/tilo-relay-state-migrate.service \
    os/usr/lib/systemd/system/tilo-network-start.service \
    os/usr/lib/systemd/system/tilo-network-start.target \
    /usr/lib/systemd/system/
COPY os/usr/lib/systemd/system/NetworkManager.service.d/20-tilo-firewall-first.conf \
    /usr/lib/systemd/system/NetworkManager.service.d/20-tilo-firewall-first.conf
COPY os/usr/lib/NetworkManager/conf.d/20-tilo-wake.conf \
    /usr/lib/NetworkManager/conf.d/20-tilo-wake.conf
COPY os/usr/lib/NetworkManager/conf.d/25-tilo-dns-cache.conf \
    /usr/lib/NetworkManager/conf.d/25-tilo-dns-cache.conf
COPY os/usr/libexec/tilo-relay-firstboot os/usr/libexec/tilo-relay-state-migrate /usr/libexec/
COPY \
    os/usr/share/polkit-1/actions/org.tilo.home-peer.policy \
    os/usr/share/polkit-1/actions/org.tilo.disk-unlock.policy \
    /usr/share/polkit-1/actions/
COPY \
    os/etc/polkit-1/rules.d/49-tilo-home-peer.rules \
    os/etc/polkit-1/rules.d/49-tilo-disk-unlock.rules \
    os/etc/polkit-1/rules.d/49-tilo-greeter-power.rules \
    os/etc/polkit-1/rules.d/49-tilo-greeter-fingerprint.rules \
    os/etc/polkit-1/rules.d/49-tilo-session-priority.rules \
    os/etc/polkit-1/rules.d/49-tilo-time-zone.rules \
    /etc/polkit-1/rules.d/
# Graphical login greeter (ui-toolkit based). Launched at the greetd VT inside
# `cage` — see etc/greetd/config.toml.
# Graphical installer. Primarily consumed by the layered installer image
# (Containerfile.installer); bundled here so it no longer needs a host build.
# App launcher + runtime-bundle CLI. `tilo-launcher` is what actually
# execs installed / declared packages inside their bwrap sandbox;
# `tilo-runtime-bundle` is the CLI over the same crate that produced
# the baked bundle at image-build time — shipping it lets operators
# `verify` or extend the bundle from a running system.
# Package management on the running system: `tilo-install` installs, lists and
# removes `.tapp`s into the user's own store, `tilo-app-package` packs and verifies
# them, and `tilo-cli-shim` is what `tilo-install` drops on $PATH for a package
# that declares CLI commands. Without these in the image the only apps a user
# could ever have are the ones the image shipped.
# The session-unlock verify helper: the compositor spawns it while the session
# is locked to verify the user's password via PAM (see os/etc/pam.d/tilo-unlock).
# It lives under /usr/libexec (not on $PATH) since only the compositor invokes
# it, and matches the compositor's DEFAULT_UNLOCK_HELPER path.
# The confined shell runtime. Apps declare `ush = "required@<v>"` under
# [sandbox.services] and the launcher refuses to start them when its socket is
# absent — apps/terminal does, so without this binary and the user unit below
# the packaged Terminal cannot launch. /usr/libexec because the terminal's own
# lookup order is $TILO_USH_BIN, then /usr/libexec/tilo-ush, then $PATH.
# AirPlay 2 mirroring (FairPlay SAP, HAP pairing, encrypted video and
# system audio) is delegated to the source-pinned, tested build above.
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    install -D -m 0644 /tilo-assets/doubletake-ae06722-LICENSE \
      /usr/share/licenses/tilo-doubletake/LICENSE \
    && install -D -m 0644 /tilo-assets/joycond-0df025a-LICENSE \
      /usr/share/licenses/joycond/LICENSE \
    && printf '%s\n' \
      'joycond - Nintendo Switch Joy-Con pairing daemon' \
      'Copyright (C) Daniel J. Ogorchock and joycond contributors' \
      'License: GPL-3.0-only (see LICENSE)' \
      'Source: https://github.com/DanielOgorchock/joycond/tree/0df025ac5dc284b1f31172b6b252321ba788c4de' \
      'Unmodified; built from the source above. /usr/lib/udev/rules.d/72-joycond.rules' \
      'is adapted from its udev/72-joycond.rules.' \
      > /usr/share/licenses/joycond/NOTICE
# PDF rendering and text extraction. Sandboxed apps read /usr/lib64 through
# the launcher's system read profile, so the viewer finds it without a grant.
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=tools/ci/install-pdfium.sh,target=/tmp/install-pdfium.sh,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    bash /tmp/install-pdfium.sh /tilo-assets /usr /tmp/tilo-assets.tsv
COPY \
    os/usr/lib/systemd/user/tilo-ush.socket \
    os/usr/lib/systemd/user/tilo-fs-recipient.service \
    os/usr/lib/systemd/user/tilo-fs-recipient.socket \
    os/usr/lib/systemd/user/tilo-fs-cloud.service \
    /usr/lib/systemd/user/
# Exercise the AirPlay CLI path that ships: PIN pairing, FairPlay setup,
# encrypted RTSP/video transport and H.264 media into the
# source-pinned receiver simulator. No receiver hardware or live network is
# involved, but an empty/control-plane-only session fails the stats assertions.
COPY os/tests/airplay-encrypted-session.sh /usr/libexec/tilo/airplay-encrypted-session-test.sh
# The session-context check proves a greetd login runs in the user's SELinux
# domain and logind session scope, with each launched app in its own scope.
COPY os/tests/session-context-check.sh /usr/libexec/tilo/
# Optional AirPlay media acceptance runs against the assembled image separately.
# Its setup/probe timing is not an installation prerequisite. Keep the full
# protocol/media/teardown test available; omitting it here does not claim success.
# Native protocol tests and the GStreamer plugin checks above remain required.
RUN chmod 0755 /usr/libexec/tilo/airplay-encrypted-session-test.sh \
          /usr/libexec/tilo/session-context-check.sh

# Put both stores' command shims on PATH, so a package that declares CLI
# commands can be run by name — the only way a CLI package is meant to be run.
COPY os/etc/profile.d/tilo-apps.sh /etc/profile.d/tilo-apps.sh

# Collaborative filesystem service. tilo-store-volume.service prepares the
# room store volume (/var/lib/tilo/store) before it, giving installs without a
# store partition an image-backed one attached by tilo-store-loop.service;
# fs.service pulls both in, so neither needs enabling.
COPY \
    os/usr/lib/systemd/system/fs.service \
    os/usr/lib/systemd/system/tilo-store-volume.service \
    os/usr/lib/systemd/system/tilo-store-loop.service \
    os/usr/lib/systemd/system/tilo-fs-start.timer \
    os/usr/lib/systemd/system/fs-identity.path \
    os/usr/lib/systemd/system/tilo-device-key-init.service \
    os/usr/lib/systemd/system/tilo-growfs-sysroot.service \
    os/usr/lib/systemd/system/tilo-growfs-sysroot.timer \
    os/usr/lib/systemd/system/tilo-firstboot-user.service \
    os/usr/lib/systemd/system/tilo-flatpak-defaults.service \
    os/usr/lib/systemd/system/tilo-cache-gc.service \
    os/usr/lib/systemd/system/tilo-cache-gc.timer \
    os/usr/lib/systemd/system/tilo-sandbox-view.service \
    os/usr/lib/systemd/system/tilo-sandbox-view.path \
    os/usr/lib/systemd/system/tilo-sandbox-view-retire@.service \
    os/usr/lib/systemd/system/tilo-runtime-resident.service \
    os/usr/lib/systemd/system/tilo-firstboot-autologin-cleanup.service \
    os/usr/lib/systemd/system/tilo-session-recovery.service \
    os/usr/lib/systemd/system/tilo-ntfs-mount.socket \
    os/usr/lib/systemd/system/tilo-ntfs-mount.service \
    os/usr/lib/systemd/system/tilo-ntfs-boot-disks.service \
    os/usr/lib/systemd/system/tilo-device-vpn.socket \
    os/usr/lib/systemd/system/tilo-device-vpn.service \
    os/usr/lib/systemd/system/tilo-device-vpn-reset.service \
    /usr/lib/systemd/system/
# NTFS drives: the kernel driver loads only when the consent-gated mount
# service asks for it, and UDisks may not mount one around that service.
COPY os/etc/modprobe.d/tilo-ntfs.conf /etc/modprobe.d/tilo-ntfs.conf
COPY os/usr/lib/tmpfiles.d/tilo-ntfs-mount.conf /usr/lib/tmpfiles.d/tilo-ntfs-mount.conf
# Your devices as a VPN (docs/device-vpn.md): the firewalld zone and policy that let the person's other
# devices' traffic out while this device is their exit, and NetworkManager
# leaving the tunnel interfaces to the helper.
COPY os/usr/lib/firewalld/ /usr/lib/firewalld/
COPY os/usr/lib/NetworkManager/conf.d/30-tilo-device-vpn.conf \
    /usr/lib/NetworkManager/conf.d/30-tilo-device-vpn.conf
COPY os/etc/polkit-1/rules.d/49-tilo-ntfs.rules /etc/polkit-1/rules.d/49-tilo-ntfs.rules
# User provisioning is needed on the installed OS as well as live media.
# disk-image.sh seeds these native units; custom installers additionally stage
# their own /etc handoff so they can install older independently signed targets.
COPY os/etc/greetd/config.toml /usr/share/tilo/greetd-config.toml
# A measured installer UKI must not try to persist its transient TPM identity
# to the read-only live ESP. These conditions stop both systemd setup phases
# only while the sealed command line carries tilo.install.
COPY os/usr/lib/systemd/system/systemd-tpm2-setup.service.d/10-tilo-live-installer.conf /usr/lib/systemd/system/systemd-tpm2-setup.service.d/10-tilo-live-installer.conf
COPY os/usr/lib/systemd/system/systemd-tpm2-setup-early.service.d/10-tilo-live-installer.conf /usr/lib/systemd/system/systemd-tpm2-setup-early.service.d/10-tilo-live-installer.conf
# The capability list that mount enforces. Shipped read-only under /usr;
# tmpfiles.d copies it to /var/lib/tilo on first boot, where it can be
# edited and survives image updates. See fs.service for why the subject
# is a principal rather than a key.
# The system interfaces this image serves sit beside it, installed by the shell
# component (os/components.toml), which judges apps against them: an app from
# the registry updates past its preinstalled copy only to a build this OS can
# run, and that copy is the fallback for one it cannot
# (crates/packaging/package-store/src/interfaces.rs).
COPY os/usr/share/tilo/fs-capabilities.json /usr/share/tilo/fs-capabilities.json
# The community workspace a computer is in by default: the vendor's site,
# which /etc/tilo/community.toml overrides (crates/session/shell, community.rs).
COPY os/usr/share/tilo/community.toml /usr/share/tilo/community.toml
# Git compares only the stat fields that sync between devices (mtime and
# size, not inode or ctime), so a synced repository is clean everywhere
# without re-hashing; see the snippet and docs/device-local-state.md.
COPY os/usr/share/tilo/git/synced-worktree.gitconfig /usr/share/tilo/git/synced-worktree.gitconfig
COPY os/etc/gitconfig /etc/gitconfig

# The one Wayland session. tilo-greeter starts it directly and shows a session
# picker only when the directory offers a real choice, so nothing else may land
# in /usr/share/wayland-sessions or /usr/share/xsessions; the image contract
# below enforces that. The separate text-mode tuigreet package is intentionally
# absent from this image.
COPY os/usr/share/wayland-sessions/shell.desktop /usr/share/wayland-sessions/
COPY --chmod=0755 \
    os/usr/libexec/tilo-security-audit \
    os/usr/libexec/tilo-firstboot-user \
    os/usr/libexec/tilo-firstboot-autologin-cleanup \
    os/usr/libexec/tilo-session-recovery \
    os/usr/libexec/tilo-cage-session \
    os/usr/libexec/tilo-clock-floor \
    /usr/libexec/
# The graphical-session user target. (`systemctl --global enable` further down
# resolves each unit file at build time, so a unit that was never copied in
# fails the build outright with "does not exist".)
COPY os/usr/lib/systemd/user/tilo-graphical-session.target /usr/lib/systemd/user/

# The echo-cancelled microphone for calls: a PipeWire client config the
# desktop session runs (`pipewire -c tilo-echo-cancel.conf`) while Settings →
# Sound → "Reduce echo in calls" is on. It lives in PipeWire's own config
# directory so /etc/pipewire/ and ~/.config/pipewire/ can override it.
COPY os/usr/share/pipewire/tilo-echo-cancel.conf /usr/share/pipewire/tilo-echo-cancel.conf

# Default compositor config plus one fixed, signed loopback camera for
# consented personal-device capture. The shell recognizes only this exact card
# label; arbitrary local V4L2 nodes are never advertised as remote receivers.
COPY --from=remote-capture-files / /

# greetd display manager config
COPY os/etc/greetd/config.toml /etc/greetd/config.toml
COPY --from=virtio-gallivm-cpu /virtio-gallivm-cpu.so /usr/lib/tilo/virtio-gallivm-cpu.so

# PAM stack for the greetd auth service (the boot sign-in first, tolerant
# pam_fprintd so a fingerprint can satisfy login; falls back to password-auth,
# and starts / unlocks the user's Secret Service for the authenticated session).
# PAM stack for in-session unlock (tilo-unlock-helper). Auth + account only.
# tilo-auth-legacy: the old password database alone, which tilo-authd checks
# once per person before moving them onto their user key.
COPY \
    os/etc/pam.d/greetd \
    os/etc/pam.d/tilo-unlock \
    os/etc/pam.d/tilo-auth-legacy \
    /etc/pam.d/
# One check for every PAM service: system-auth and password-auth, which the
# greeter, the lock screen, polkit, sudo, su and passwd all include, ask
# tilo-authd (pam_tilo) before the old password hash. A custom authselect
# profile over the configured one, with its features, so authselect keeps
# owning the files; an image whose base has no authselect configuration fails
# here rather than shipping a stack that skips the daemon.
RUN --mount=type=bind,source=os/scripts/authselect-tilo.py,target=/run/authselect-tilo.py,ro \
    current="$(authselect current --raw)" \
    && base="${current%% *}" \
    && features="$(printf '%s' "$current" | cut -s -d' ' -f2-)" \
    && authselect create-profile tilo --base-on="$base" \
    && python3 /run/authselect-tilo.py /etc/authselect/custom/tilo \
    && authselect select custom/tilo $features --force \
    && grep -q pam_tilo.so /etc/pam.d/system-auth \
    && grep -q pam_tilo.so /etc/pam.d/password-auth

# polkit rule authorising the greeter's Reboot / Shut down buttons. Lets the
# `greetd` user issue org.freedesktop.login1.{reboot,power-off} via logind
# from the pre-login screen (see the rule's header for the rationale).

# Narrow fprintd account-selection grant for the pre-login `greetd` user.
# Verification remains in PAM; enrollment/deletion stays privileged.

# udev rule handing /sys/class/backlight/*/brightness to the `video`
# group, so the compositor's brightness keys and quick-settings slider
# can actually dim the panel from an unprivileged session. This is the
# rule `brightnessctl` would have brought with it; Tilo writes the sysfs
# attribute directly (no fork per keypress — brightness keys autorepeat)
# and therefore ships the rule rather than the package.
#
# 70-tilo-console.rules hands a VM's console port (org.tilo.console.0, which
# only a hypervisor can attach) to the greetd user, whose greeter answers the
# tilo-control action protocol there (crates/session/console-control).
#
# 70-tilo-ddc.rules hands the display controllers' I²C buses to the active
# session so external monitors' brightness can be set over DDC/CI; the
# tilo-ddc.conf modules-load snippet (copied with the remote-camera one)
# makes i2c-dev expose them as /dev/i2c-N.
#
# 72-joycond.rules hands raw Joy-Con halves to joycond (installed with the
# desktop binaries), which presents a pair as one combined controller;
# Pro/SNES/N64 controllers stay with hid-nintendo and Steam.
COPY os/usr/lib/udev/rules.d/90-tilo-backlight.rules os/usr/lib/udev/rules.d/60-tilo-io-scheduler.rules os/usr/lib/udev/rules.d/70-tilo-console.rules os/usr/lib/udev/rules.d/70-tilo-ddc.rules os/usr/lib/udev/rules.d/72-joycond.rules /usr/lib/udev/rules.d/

# Grow the block-backed filesystem at /sysroot. The stock systemd-growfs-root.service
# targets `/`, which is a composefs overlay here and cannot be grown; the
# drop-in stands it down on this layout so the two never both apply.
COPY os/usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf /usr/lib/systemd/system/systemd-growfs-root.service.d/10-tilo-composefs.conf
# The ESP random seed refresh runs only where /boot is writable; Tilo's is not.
COPY os/usr/lib/systemd/system/systemd-boot-random-seed.service.d/10-tilo-read-only-boot.conf /usr/lib/systemd/system/systemd-boot-random-seed.service.d/10-tilo-read-only-boot.conf

# systemd-tmpfiles snippets — pre-create /var/workspace and
# /var/cache/quick-fs at boot. bootc images mount the root r/o, so
# this is the only sanctioned way to materialise persistent dirs.
COPY os/usr/lib/tmpfiles.d/tilo.conf os/usr/lib/tmpfiles.d/tilo-relay.conf os/usr/lib/tmpfiles.d/tilo-device-vpn.conf /usr/lib/tmpfiles.d/
# systemd-tmpfiles --user reads /usr/share/user-tmpfiles.d, not /usr/lib.
COPY os/usr/share/user-tmpfiles.d/tilo-model-manager.conf os/usr/share/user-tmpfiles.d/tilo-fs.conf os/usr/share/user-tmpfiles.d/tilo-secret-service.conf /usr/share/user-tmpfiles.d/

# The device-trust group, which must exist before tmpfiles-setup runs so
# the identity directory can be created with it. See the snippet for why
# the device key is group-owned rather than owned by one component.
# The relay's static account: a desktop set up as a node runs the relay
# (tilo-node-relay@) under it, as the host image runs tilo-relay.service. Its
# snippet ships with the relay component (os/components.toml), which arrives
# with the compiled binaries, so the account is made from the source file here.
COPY \
    os/usr/lib/sysusers.d/tilo-local-model.conf \
    os/usr/lib/sysusers.d/tilo-identity.conf \
    os/usr/lib/sysusers.d/tilo-ntfs.conf \
    /usr/lib/sysusers.d/
RUN --mount=type=bind,source=os/usr/lib/sysusers.d/tilo-relay.conf,target=/run/tilo-relay-sysusers.conf,ro \
    systemd-sysusers /usr/lib/sysusers.d/tilo-identity.conf \
 && systemd-sysusers /usr/lib/sysusers.d/tilo-local-model.conf \
 && systemd-sysusers /run/tilo-relay-sysusers.conf \
 && systemd-sysusers /usr/lib/sysusers.d/tilo-ntfs.conf

# xdg-desktop-portal backend registration.
#
# One file now: tilo.portal, which tells xdg-desktop-portal which
# interfaces we provide and what `UseIn` desktop string activates us.
# The compositor itself owns `org.freedesktop.impl.portal.desktop.tilo`
# on the session bus — see `compositor::native::start_portal_backend`.
#
# The D-Bus activation file and shell-portal.service used to start a
# separate `shell --portal-server-only` process to own that name. They
# are gone, for two reasons. That process built its portal context on an
# empty WorkspaceTree, so it could not answer a single question about
# the running session; and D-Bus allows one owner per name, so with the
# compositor also serving, whichever lost the race left a portal that
# answered nothing while looking healthy.
COPY os/usr/share/xdg-desktop-portal/portals/tilo.portal /usr/share/xdg-desktop-portal/portals/tilo.portal
COPY os/usr/share/xdg-desktop-portal/tilo-portals.conf /usr/share/xdg-desktop-portal/tilo-portals.conf

# OS infra config (C3 zram swap, C4 locale) and the suspend policy that
# freezes user sessions before the kernel freezer reaches processes blocked
# on the userspace collaborative filesystem.
COPY os/etc/systemd/ /etc/systemd/
COPY os/etc/locale.conf /etc/locale.conf
COPY os/etc/chrony.conf /etc/chrony.conf
COPY crates/session/install-profile-model/locales.txt /usr/share/tilo/installer-locales.txt

# Bundled documentation (the README in the OS submodule, plus the
# peer-sync transport and diagnostics doc).
# The Secure Boot + disk-encryption procedure. On-image because the enrolment
# sequence has to be readable from the machine being enrolled, which is the one
# place a wrong step stops it from booting.
# The boot-chain document the Secure Boot units and tilo-security-audit point at.
# It used to be copied from docs/secure-boot.md, which was folded into
# docs/security/ — and the COPY was not updated, so every image build since has
# failed at this line with `"/docs/secure-boot.md": not found`. Installed under its
# new name, and the Documentation= lines that referenced the old one follow it.
COPY \
    os/docs/peer-sync.md \
    docs/security/boot-chain.md \
    docs/security/rollback.md \
    /usr/share/doc/tilo/

# The install-config drop-in that flips bootc's bootloader choice to
# systemd-boot. Lives at /usr/lib/bootc/install/*.toml — a drop-in dir bootc
# merges at every `bootc install {to-disk,to-filesystem}` invocation. bib
# runs the same install code path, so this covers the disk-image step
# (os/dev/disk-image.sh) and the installer's `bootc install to-disk` alike.
COPY os/usr/lib/bootc/install/40-tilo.toml /usr/lib/bootc/install/40-tilo.toml

# ---------------------------------------------------------------------------
# Kernel/userspace hardening (security plan §11 — "table stakes")
# ---------------------------------------------------------------------------
# Kernel args (init_on_alloc, lockdown=integrity, slab_nomerge, kstack
# randomisation, page-alloc shuffle, vsyscall=none). bootc applies kargs.d at
# install/upgrade, and `bootc container ukify` seals it into the UKI command line
# at build time — which is what makes this set measured rather than merely
# applied. 03-boot.toml carries the arguments the install step used to inject.
# 05-credentials.toml refuses systemd credentials from firmware, fw_cfg, the
# boot loader or an unsigned command line.
COPY \
    os/usr/lib/bootc/kargs.d/01-hardening.toml \
    os/usr/lib/bootc/kargs.d/00-root.toml \
    os/usr/lib/bootc/kargs.d/03-boot.toml \
    os/usr/lib/bootc/kargs.d/04-mode.toml \
    os/usr/lib/bootc/kargs.d/05-credentials.toml \
    /usr/lib/bootc/kargs.d/
# Which mode the sealed command line boots into. `tilo.mode=repair` is what the
# repair entry carries, and it can only come from inside the signature — see the
# file's own header for why the two values have to stay the same length.

# Disk-encryption block work: partitioning + LUKS2 + unlock-factor enrolment.
# Used by the installer (os/Containerfile.installer layers tilo-install on top of
# this image), and afterwards on the installed system — `enrol` is also how a
# volume is re-sealed to a replaced TPM, which is exactly the case where the
# machine can only be reached with the recovery key. tilo-store-volume plans and
# formats the room store partition for the installer and runs the store at boot.
COPY --chmod=0755 \
    os/usr/libexec/tilo-disk-encrypt \
    os/usr/libexec/tilo-store-volume \
    os/usr/libexec/tilo-verified-boot-install-preflight \
    /usr/libexec/

# Owner-enrolled Secure Boot. Tilo builds its own kernel, so the base image's
# Fedora/Microsoft-signed chain cannot cover it; the machine generates its own
# Platform Key instead, which is also what keeps a self-built image attestable
# to a collaborator who trusts that key.
#
# Generating and enrolling keys is a deliberate act the owner takes — nothing
# here does it, and the image build must never do it. What IS enabled by default
# is the re-signing unit, so the hook is already in place before any key is
# enrolled; it is a no-op until keys exist (ConditionPathExists). Getting that
# order wrong — keys enrolled, signing hook missing — is what leaves a machine
# refusing its own kernel.
COPY os/usr/libexec/tilo-secureboot /usr/libexec/tilo-secureboot
COPY \
    os/usr/lib/systemd/system/tilo-secureboot-sign.service \
    os/usr/lib/systemd/system/tilo-secureboot-sign.timer \
    os/usr/lib/systemd/system/tilo-secureboot-sign.path \
    os/usr/lib/systemd/system/tilo-secureboot-sign-shutdown.service \
    /usr/lib/systemd/system/
RUN chmod 0755 /usr/libexec/tilo-secureboot \
    && systemctl enable tilo-secureboot-sign.timer tilo-secureboot-sign.path \
                        tilo-secureboot-sign-shutdown.service

# Runtime tunables: no cross-process debugging, no kernel pointers or ring
# buffer for unprivileged readers, no unprivileged BPF, plus the /tmp
# symlink-race and LAN-spoofing settings. Under
# /usr/lib because it is part of the image; /etc/sysctl.d/ with a higher number
# still wins for a machine that needs to differ. The file's header explains why
# unprivileged user namespaces are NOT disabled — bubblewrap, hence the whole
# app sandbox, needs them.
COPY os/usr/lib/sysctl.d/60-tilo-hardening.conf /usr/lib/sysctl.d/60-tilo-hardening.conf
COPY os/usr/lib/sysctl.d/70-tilo-performance.conf /usr/lib/sysctl.d/70-tilo-performance.conf

# Which sandboxed packages get the expensive protections (hardened allocator +
# hardware shadow stacks). Read by tilo-launcher at launch; scoped rather than
# image-wide because the allocator costs multiples on allocation-heavy work and
# most of this system is Rust, which does not produce the bugs it catches. The
# image owns the list — an app manifest can neither request nor decline it.

# Which sandboxed packages may map memory writable and executable at once.
# Read by tilo-launcher when it compiles the syscall filter. Same reasoning as
# the file above and the same format: a package that could name its own
# profile would be lifting a protection off itself, so the image decides and
# the manifest only asks. web-engine-entitled.conf names the packages that may
# create the namespaces of a web engine's own renderer sandbox.
COPY \
    os/usr/lib/tilo/untrusted-parsers.conf \
    os/usr/lib/tilo/jit-entitled.conf \
    os/usr/lib/tilo/web-engine-entitled.conf \
    /usr/lib/tilo/

# The boot self-check that ASSERTS what's actually active.
COPY \
    os/usr/lib/systemd/system/tilo-security-audit.service \
    os/usr/lib/systemd/system/tilo-security-audit.timer \
    os/usr/lib/systemd/system/tilo-pstore-console.service \
    os/usr/lib/systemd/system/tilo-clock-floor.service \
    os/usr/lib/systemd/system/tilo-clock-floor.timer \
    os/usr/lib/systemd/system/tilo-clock-floor-shutdown.service \
    /usr/lib/systemd/system/
# tilo-clock-floor keeps the clock floor PID 1 starts the clock from
# (/var/lib/systemd/timesync/clock) at chrony's authenticated time, so a
# real-time clock that comes up behind never starts the system before the last
# recorded time. Its units ride the COPY above, the helper the libexec COPY.
# A reset leaves its kernel log in pstore on boards that reserve a ramoops
# region (os/kernel/patches/0021); systemd-pstore moves it into the journal on
# the next boot, and tilo-pstore-console.service makes the console log in it
# carry more than critical messages.
COPY os/usr/lib/systemd/pstore.conf.d/10-tilo.conf /usr/lib/systemd/pstore.conf.d/10-tilo.conf

# ---------------------------------------------------------------------------
# Bounded rollback protection
# ---------------------------------------------------------------------------
# The last few OS generations may unseal the disk; older ones may not. A TPM2 NV
# monotonic counter holds the OLDEST permitted generation, and each generation's
# LUKS2 keyslot is sealed to a policy requiring the floor to be at or below it.
# The generation this image carries is written near the end of this stage (see
# "The generation" there).

COPY --from=rollback-files / /
RUN chmod 0755 /usr/libexec/tilo-rollback-common /usr/libexec/tilo-rollback-confirm \
        /usr/libexec/tilo-rollback-initrd-unlock \
        /usr/libexec/tilo-root-discover \
        /usr/lib/systemd/system-generators/tilo-rollback-unlock-generator \
        /usr/lib/systemd/system-generators/tilo-root-generator \
        /usr/lib/dracut/modules.d/91tilo-rollback/module-setup.sh \
        /usr/lib/dracut/modules.d/92tilo-sibling-unlock/module-setup.sh \
        /usr/libexec/tilo-rollback-harden /usr/libexec/tilo-rollback-boot-check \
        /usr/libexec/tilo/boot-units-gate.sh \
 && install -d -m 0755 /usr/share/tilo/pcr-predictions

# ---------------------------------------------------------------------------
# Getting back from a bad update: boot assessment, rollback, repair mode
# ---------------------------------------------------------------------------
# The rollback machinery above narrows which generations may unseal the disk. This
# is the other half — the part that notices a generation is bad and gets the
# machine off it.
#
#   tilo-boot-entries.timer     schedules non-blocking maintenance that gives every
#                              not-yet-proven deployment a systemd-boot try counter
#                              and derives the repair entry.
#   tilo-boot-health.service    ordered before boot-complete.target, so a boot that
#                              never reaches a signed-in session with working input
#                              is never blessed and the counter it spent stays spent.
#   tilo-boot-fallback.service  after the boot loader went back from a deployment
#                              that failed its tries, makes the running one bootc's
#                              default and writes what failed; the session's
#                              tilo-boot-fallback-notice tells the person.
#   tilo-boot-mode-generator    turns `tilo.mode=repair` on the SEALED command line
#                              into a boot that comes up at the repair console
#                              instead of the desktop.
#
# `bootc rollback` is the command that should do this and it is not dependable on
# the composefs backend (bootc#2208), so /usr/libexec/tilo-deployment verifies what
# it did and falls back to the boot loader's own default entry.
#
# tilo-uki-cmdline (tilo-esp-write's UKI editor) ships because the repair entry is DERIVED from
# the installed UKI rather than built: a second UKI inside the rootfs would change
# the composefs digest it has to name. It rewrites `tilo.mode=normal` to
# `tilo.mode=repair` in place, same length, so every following PE section keeps its
# offset and the image still loads.
# The user-facing command. Thin: the work is in tilo-deployment, and this is the
# part that decides how to become root to run it — directly when already root,
# through the polkit-authorised unit from a desktop session, and saying to use
# sudo rather than failing with a permission error from three layers down.
COPY --from=recovery-files / /
# The extension units are enabled with the other system units below. Both are
# inert until the owner opts in: the guard runs only when something is merged
# or opted in, and the helper's refresh is a no-op without an opt-in.
COPY --from=extension-files / /
# The narrow polkit grant that makes Settings' rollback control and its automatic
# -updates switch able to do what they say. See the rule for how narrow.
# The boot-side check, bundled so it can be re-run on a live machine. It is the
# only place several of these facts can be established at all — see its header.

# The signed PCR predictions this image was built with go in
# /usr/share/tilo/pcr-predictions/generation-<N>*.json, produced by
# `tilo-rollback sign-prediction` on the build host — the private half of
# keys/tpm2-pcr-signing.pem is deliberately not in the image, so a device cannot
# sign its own expected measurements. This layer is necessarily empty because
# the final composefs-resealed UKI does not exist yet. build-image.sh measures
# that exact UKI with its paired systemd-measure and adds authority-verified
# prediction sidecars in a /boot-only layer; published builds fail closed if the
# protected signer is unavailable.

ARG TILO_WITH_WINDOWS
LABEL org.tilo.with-windows="${TILO_WITH_WINDOWS}"
COPY --from=windows-provider /out/usr/ /usr/
# The payload is validated where it is packed (windows-compat-sysext); this
# image only checks the record it carries is the one that payload was built from.
RUN --network=none --mount=type=bind,source=os/dev/windows_provider.py,target=/tilo-windows-provider.py,ro \
    python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d["provider"] in ("none","fex-traditional-wine","native-wine"), d["provider"]' \
      /usr/share/tilo/windows/provider.json
# Windows compatibility on demand: the FEX payload, the x86 guest root, Proton,
# the Steam runtime and the virtio-win media (about 3.1 GB, a quarter of /usr)
# are NOT in this image. They are the signed system extension built by the
# windows-compat-sysext stage, which a person installs with an explicit
# action the first time they enable Windows apps or create a Windows VM
# (tilo-extension install:windows-compat). It overlays the same /usr paths, so
# nothing that names them changes. What stays here is small: the UMU launcher,
# the catalogues, the provider record with the payload's pinned hashes, and the
# contracts the host needs to explain an absent payload.
COPY --from=windows-compat-artifacts /out/bin/umu-run /usr/libexec/tilo-umu-run
COPY --from=windows-compat-artifacts /out/bin/umu_run.py /usr/libexec/umu_run.py
COPY --from=windows-compat-artifacts /out/catalogues/ /usr/share/tilo/windows/catalogues/
COPY os/usr/share/tilo/windows/fex-emulator.json os/usr/share/tilo/windows/fex-graphics-provider.json /usr/share/tilo/windows/
# The VM is defined only after an owner supplies installation media. These
# immutable host/guest contracts deliberately ship no Windows image or key.
COPY \
    os/usr/libexec/tilo-windows-vm-network \
    os/usr/libexec/tilo-windows-provision \
    os/usr/libexec/tilo-windows-owner \
    os/usr/libexec/tilo-windows-recovery-console \
    /usr/libexec/
COPY os/usr/share/tilo/windows/ /usr/share/tilo/windows/
COPY os/usr/share/polkit-1/actions/org.tilo.windows.policy /usr/share/polkit-1/actions/org.tilo.windows.policy
COPY os/etc/polkit-1/rules.d/49-tilo-windows.rules /etc/polkit-1/rules.d/49-tilo-windows.rules
COPY \
    os/usr/lib/systemd/system/tilo-windows-vm-network.service \
    os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.service \
    os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.timer \
    os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.path \
    os/usr/lib/systemd/system/tilo-windows-vm-egress-lease.path \
    /usr/lib/systemd/system/
COPY \
    docs/windows-vm.md \
    docs/windows-presentation.md \
    /usr/share/doc/tilo/
RUN chmod 0755 /usr/libexec/tilo-windows-vm-network /usr/libexec/tilo-windows-provision /usr/libexec/tilo-windows-owner \
              /usr/libexec/tilo-windows-recovery-console \
    && systemctl enable tilo-windows-vm-egress-reaper.path tilo-windows-vm-egress-lease.path

# ---------------------------------------------------------------------------
# Enable services
# ---------------------------------------------------------------------------
# The portal backend needs no unit: the compositor serves it in-process,
# so it starts and stops with the session it answers for.
# `tilo-ush.socket` is globally enabled: every app declaring
# `ush = "required@<v>"` is refused without that endpoint, while socket
# activation keeps the supervisor itself off the login path until first use.
#
# Keep service activation in one transaction. On rootless Podman, each RUN
# otherwise commits a tiny layer by walking the complete bootc rootfs; measured
# cycle-6 commits took 90--120 seconds apiece. This remains late in the file so
# all unit-presence checks are unchanged, while earlier package/builder layers
# retain their useful cache boundaries.
#
# C6 audio: enable the PipeWire user stack for every account (socket-activated
# pipewire + pipewire-pulse, plus the wireplumber session manager). Printing is
# socket-activated rather than always-on. Time sync, firewall, Bluetooth,
# homed, hardening, boot assessment, automatic bootc updates and
# NetworkManager are system services/timers. joycond is not enabled here:
# 72-joycond.rules pulls it in when a Joy-Con appears, so it stays off the
# boot path and off machines that never see one.
RUN systemctl enable greetd.service && \
    systemctl disable NetworkManager.service firewalld.service chronyd.service && \
    # Fedora's presets start these at boot, ordered before multi-user.target or
    # graphical.target. They start on demand instead: ModemManager and
    # power-profiles-daemon when a client calls their D-Bus names, virtqemud
    # from its sockets. Removing only the target wants keeps the D-Bus aliases
    # and sockets that activate them.
    rm /etc/systemd/system/multi-user.target.wants/ModemManager.service \
       /etc/systemd/system/graphical.target.wants/power-profiles-daemon.service \
       /etc/systemd/system/multi-user.target.wants/virtqemud.service && \
    systemctl enable tilo-device-key-init.service && \
    systemctl enable tilo-flatpak-defaults.service && \
    # Fedora's remote-add starts with tilo-flatpak-defaults, off the greeter's path; enabled
    # on its own, multi-user.target and so graphical.target would wait for it.
    systemctl disable flatpak-add-fedora-repos.service && \
    systemctl enable tilo-cache-gc.timer && \
    systemctl --global enable tilo-cache-gc.timer && \
    systemctl --global enable tilo-app-update.timer && \
    systemctl --global enable tilo-public-content-refresh.timer && \
    systemctl --global enable tilo-runtime-resident.service tilo-runtime-resident.path && \
    systemctl set-default graphical.target && \
    systemctl enable tilo-growfs-sysroot.timer && \
    systemctl enable tilo-fs-start.timer fs-identity.path && \
    systemctl enable tilo-system-extension-dispatcher.socket && \
    systemctl enable tilo-ntfs-mount.socket tilo-ntfs-boot-disks.service && \
    systemctl enable tilo-device-vpn.socket tilo-device-vpn-reset.service && \
    systemctl enable tilo-system-extension-boot-pack-status.timer && \
    systemctl enable tilo-system-extension-boot-pack-stage.path && \
    systemctl --global enable tilo-models.socket && \
    systemctl --global enable tilo-model-gateway.socket && \
    systemctl --global enable tilo-projector.socket && \
    systemctl --global enable tilo-app-host.socket && \
    systemctl --global enable tilo-ush.socket && \
    systemctl --global enable tilo-fs-recipient.socket && \
    systemctl --global enable tilo-fs-cloud.service && \
    systemctl --global enable pipewire.socket pipewire-pulse.socket wireplumber.service && \
    systemctl enable cups.socket && \
    systemctl enable tilo-security-audit.timer && \
    systemctl enable tilo-clock-floor.timer tilo-clock-floor-shutdown.service && \
    systemctl enable systemd-pstore.service tilo-pstore-console.service && \
    systemctl enable tilo-authd.service && \
    systemctl enable tilo-rollback-harden.timer && \
    systemctl enable tilo-rollback-confirm.timer && \
    systemctl enable tilo-boot-entries.timer tilo-boot-health.service tilo-boot-fallback.service && \
    systemctl --global enable tilo-boot-fallback-notice.service && \
    systemctl enable tilo-deployment-status.socket && \
    systemctl enable bootc-fetch-apply-updates.timer && \
    systemctl enable tilo-network-start.service && \
    systemctl enable tilo-extension-activate.service tilo-extension-refresh.timer && \
    systemctl enable tilo-sandbox-view.service tilo-sandbox-view.path && \
    systemctl enable tilo-runtime-resident.service && \
    systemctl enable NetworkManager-dispatcher.service

# Boot-time prune. These units sit on the critical chain but their work
# is either irrelevant to a desktop Tilo (offline man-db caching), or a
# `Wants=network-online.target` wait we don't need (nothing on the boot
# path blocks on a routable interface — the shell + peer sync tolerate
# late network). Masking (not just disabling) makes them
# unable to be pulled in by transitive `Wants=`.
#
# `|| true` because the unit is only present when its package is
# installed — we don't fail the image build if a mask target is absent.
# Fedora also presets systemd-pcrlock collectors at sysinit. Tilo does not use
# pcrlock credentials: its encrypted-root policy is the authority-signed PCR11
# generation flow in tilo-rollback. Running both performs redundant TPM/NV and
# filesystem scans before login and creates a second policy state machine.
# The sealed /usr takes no system or configuration extension at boot. Fedora
# presets systemd-sysext and systemd-confext, which merge every image or plain
# directory under /etc, /run and /var/lib/{extensions,confexts} with systemd's
# default image policy, which accepts unsigned images. A file written to /var
# would then overlay the composefs-sealed /usr, or /etc, on the next boot.
# systemd has no global setting that refuses directories or unsigned images,
# so the image merges nothing itself; a caller merging an extension on purpose
# passes its own `--image-policy`. Unlike the pcrlock and prune masks,
# these units must exist: a missing unit is a build failure, not a skip. The
# unsealed dev image unmasks systemd-sysext for its development layers.
# systemd defines NvPCRs (TPM NV indices extended like PCRs: "hardware" for
# the product ID, "cryptsetup" for crypttab's tpm2-measure-keyslot-nvpcr=)
# and anchors each one on every boot with a secret it keeps as a TPM-sealed
# credential in /var/lib/systemd/nvpcr and in the ESP's loader/credentials.
# Nothing in Tilo extends, reads or seals to an NvPCR (the encrypted root's
# policy is tilo-rollback's signed PCR 11 flow). Left on, systemd-tpm2-setup
# fails on every boot after the first, because the ESP is mounted read-only
# and the copy for the initrd cannot be written, and each boot spends TPM
# sessions anchoring them. Masking the definitions turns the whole mechanism
# off; systemd-pcrproduct, which only extends the "hardware" NvPCR, goes with
# them.
RUN for u in \
        NetworkManager-wait-online.service \
        systemd-networkd-wait-online.service \
        systemd-network-generator.service \
        systemd-timesyncd.service \
        systemd-coredump.socket \
        systemd-coredump@.service \
        systemd-binfmt.service \
        systemd-pcrlock-file-system.service \
        systemd-pcrlock-firmware-code.service \
        systemd-pcrlock-firmware-config.service \
        systemd-pcrlock-machine-id.service \
        systemd-pcrlock-make-policy.service \
        systemd-pcrlock-secureboot-authority.service \
        systemd-pcrlock-secureboot-policy.service \
        man-db-cache-update.timer \
        man-db-cache-update.service \
        systemd-update-utmp.service \
    ; do systemctl mask "$u" || true; done \
 && systemctl mask systemd-sysext.service systemd-confext.service systemd-sysext.socket \
 && for u in systemd-sysext.service systemd-confext.service systemd-sysext.socket; do \
      test "$(readlink "/etc/systemd/system/$u")" = /dev/null; \
    done \
 && mkdir -p /etc/nvpcr \
 && for f in /usr/lib/nvpcr/*.nvpcr; do ln -sfn /dev/null "/etc/nvpcr/${f##*/}"; done \
 && systemctl mask systemd-pcrproduct.service \
 && test "$(readlink /etc/nvpcr/hardware.nvpcr)" = /dev/null \
 && test "$(readlink /etc/nvpcr/cryptsetup.nvpcr)" = /dev/null \
 && test "$(readlink /etc/systemd/system/systemd-pcrproduct.service)" = /dev/null

# C5 firewall policy. Default to the desktop zone (FedoraWorkstation), whose
# 1025-65535 UDP range covers Iroh's ephemeral QUIC endpoint. Invites carry
# authenticated endpoint addresses, so collaboration needs no multicast
# discovery service or fixed inbound port.
# firewalld isn't running at build time, so edit the permanent config offline.
#
# Printing adds NO service here, deliberately. `ipp` (631/tcp) would expose
# the scheduler, and `ipp-client` would open the door for the DNS-SD printer
# announcements Tilo does not listen for. Printing on this image is outbound
# only — the desktop talks to a printer, nothing talks to the desktop — and
# the FedoraWorkstation zone already permits the outbound side. Adding either
# service is a decision to be argued for, not a default.
RUN firewall-offline-cmd --set-default-zone=FedoraWorkstation
