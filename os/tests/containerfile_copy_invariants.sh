#!/usr/bin/env bash
# Static proof that rootfs COPY consolidation changes layer shape, not payload.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
containerfile="$repo/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$here/lib/containerfile.sh"

# Public trust now stages the selected keys, independent tracked references,
# and three validation tools under /run before installing validated bytes.
# test_public_trust_install.py proves default keys and development provenance;
# the contract below covers these explicit staging inputs plus existing payload.
# Captured from the effective rootfs copy mappings. The current change
# deliberately removes the obsolete root-owned shell-firstboot unit and adds
# non-blocking boot timers, a background-resource policy, and two model-service
# activation sockets, the staged-image owner trust gate, and the deferred
# BootPack status-projection timer, the physical-disk BFQ policy, the
# root-owned device-key initializer, the off-critical-path TPM hardening timer,
# the authenticated Chrony policy used by the image-trust freshness floor, the
# zero-delay firewall-first network target that keeps both services off the
# login chain without exposing an unfiltered interface, and
# the packaged-app external AT-SPI client and installed-app accessibility gate,
# the build-only residual OpenSSL ABI package gate (removed after it runs),
# the xdg-desktop-portal preference that selects the Tilo backend,
# and the opt-in extension helper overlay (helper, units, polkit rule,
# and the NetworkManager dispatcher hook that starts a deferred install).
# The redundant model-storage oneshot is now absent; systemd provisions the
# model and gateway directories directly without a helper on socket activation.
# Rustls now compiles the fixed CA-bundle path
# into its loader, so the former systemd environment override is absent.
# Tilo build outputs are installed through os/components.toml, so their rows
# name the manifest component and build output instead of a builder /out path;
# every destination is unchanged. The VM console
# port udev rule and the generated bash/fish/zsh completions for app and tilo
# (with their store/system aliases) are deliberate additions, as is the
# sandboxed PDF engine helper at /usr/libexec/tilo-pdf-helper, and the
# synced-worktree git defaults (/etc/gitconfig and its snippet) are part of the
# payload, as is the room store volume: its helper, the unit that prepares it
# before fs.service and the loop unit of image-backed stores. The signature
# attachment lookups for the code.tilo.so OS repository and the
# registry.tilo.so owner namespace are staged with the other trust inputs; the
# former code-host lookup is gone. The PipeWire client config behind the
# echo-cancelled microphone for calls is part of the payload. The DDC/CI udev
# rule and its i2c-dev modules-load snippet, which let the session set external
# monitors' brightness, are deliberate additions. So are the cloud account
# service, tilo-fs-cloud.service, and its vault credential helper at
# /usr/libexec/tilo-account-credential. The finished-rootfs UI toolkit gate is
# a deliberate addition. The runtime NetworkManager resolver symlink is
# restored after bootc transient cleanup, before split/ukify seal the rootfs.
# The build-only DNF/RPM clock shim and transaction-state cleaner add their
# explicit rootfs COPY rows; the shim and wrappers are removed before sealing.
# joycond's binary, unit and modules-load snippet (through desktop-binaries)
# and its Joy-Con-scoped udev rule are deliberate additions. So is
# /usr/lib/tilo/web-engine-entitled.conf, the image-owned list of packages
# the launcher lets build a web engine's own renderer sandbox. The
# tilo-identity.service user unit (a one-shot chain-v1 repair) is gone; the
# compositor verifies and repairs identity at startup. The session flight
# recorder and tilo-session-report (through desktop-binaries) are deliberate
# additions: the image's tilo-session now records each login. The
# systemd-boot-random-seed drop-in, which skips the seed refresh on a
# read-only /boot, is a deliberate addition. The rootfs
# takes its build outputs from native-builder-payload, the builder's outputs
# copied unchanged, so it no longer needs the compile stage's intermediate
# cache.
# tilo-esp-write and its unit, the only writer of the read-only ESP, are
# deliberate additions. The helper is now compiled: it arrives from the
# esp-write component at the same path and mode instead of from the context.
# The virtio-only Cage session wrapper is installed as a single executable;
# physical GPU sessions still use the ordinary greeter command.
# tilo-hidepid.service is a deliberate removal: a global hidepid /proc hides
# PID 1 from unprivileged processes and breaks libsystemd unit lookups.
# user-tmpfiles.d/tilo-fs.conf is a deliberate addition: it creates the
# filesystem services' roots, which their PrivateIPC sandbox cannot.
# user-tmpfiles.d/tilo-secret-service.conf is a deliberate addition: it
# creates the vault directory the secret service's read-only home names in
# ReadWritePaths=, which must exist before the unit can start.
# The user-tmpfiles.d snippets live in /usr/share, the only system path
# systemd-tmpfiles --user reads.
# The native runtime host at /usr/libexec/tilo/tilo-runtime-mcp is a deliberate addition.
# /usr/bin/bootc from bootc-builder, the base's release patched to find this
# system's ESP on a disk it shares, and its licence notice are deliberate.
# tilo-pstore-console.service and the systemd-pstore drop-in that keeps pstore
# records in the journal are deliberate additions.
# Sorting
# intentionally ignores instruction order:
# destinations are unique, and the source-stage/source/destination triple defines
# each COPY's installed bytes and path. Modes remain source modes except at the
# explicitly equivalent COPY --chmod=0755 sites checked below. COPY preserves
# ownership, mtimes and symlinks through scratch stages; no RUN mutates them.
# ARM virtio software rendering adds one reviewed LLVM CPU shim payload.
# The extension overlay replaces the codec-specific resolver: no repository
# definitions or upstream keys ship, only the generic helper and its units.
# The build-only Cisco OpenH264 gate is a deliberate addition (removed after it runs).
# The extension-manifest public key ships beside the helper.
# The release gate's greetd session-context check ships beside the other
# booted-guest checks in /usr/libexec/tilo.
# The image ships one login session and no peer-sync marker unit.
# The release gate's input hotplug check and the device admin's action-model
# check ship beside them.
# The boot units gate ships with the rollback units whose confirmation it gates.
# The ush supervisor ships with what it runs: the two relay helpers for
# Terminal, and the legacy VM service behind app shims. The WPE engine overlay
# and /usr/bin/browser are deliberate removals: the Browser is a tapp in the
# app store and its Chromium engine a signed runtime bundle under
# /usr/lib/tilo-runtime, both already copied.
# NetworkManager's Wake-on-LAN default ships as a conf.d drop-in.
# The sibling-unlock client (/usr/libexec/tilo/tilo-sibling-unlock, through
# components.toml) and its dracut module ship in the rollback-files overlay
# beside tilo-rollback, which the initramfs carries the same way.
# The registry's runtime publisher key (/usr/share/tilo/registry-runtime-
# publisher.hex), which the launcher trusts for runtimes the Store installs,
# is a deliberate addition.
# /usr/bin/terminal, /usr/bin/file-explorer and /usr/bin/settings are
# deliberate removals: those apps ship only as packages in the app store.
# The system-interface declaration (/usr/share/tilo/system-interfaces.toml),
# which says what app builds this OS can run, is a deliberate addition.
# The ush service unit is installed through its component manifest so normal
# component updates carry the same owned reexec path as the OS image.
# The public /usr/bin/ush symlink deliberately exposes the same confined
# runtime already installed at /usr/libexec/tilo-ush.
# The confined speech worker and its full MIT notice are deliberate additions.
# The tilo-projector.service and .socket user units, which run the projection
# broker as a socket-activated service in place of per-command helper runs, are
# deliberate additions.
# Cisco requires the complete OpenH264 binary license notice in a licensing
# information location. Only that text is added to extension-files; Cisco
# binaries remain separately downloaded by opted-in devices.
# The offline, signed Flathub descriptor is the one additional payload row.
# The AppHost service unit moves from a direct context COPY into its component
# manifest; the installed path, reviewed bytes and 0644 mode are unchanged.
# Firstboot account handoff and authenticated startup recovery now ship in the
# base image too: their three helpers, three units and the greeter baseline.
# The session wrapper moves into the shell component beside its compositor;
# the destination, source bytes and executable mode remain the same.
# The disk-unlock polkit action and exact-command read rule ship beside the
# existing policies. Their two rootfs payload rows are the intentional delta.
# The version-bound cooperative Bash editor at /usr/lib/tilo/ush/bash-editor.so
# is an intentional ush component payload addition; no permission is activated.
# The signed SDK font-catalog tree adds one native-builder-payload row.
# Combining the application publisher with the existing runtime publisher
# COPY preserves their exact payload mappings and the 156-instruction budget.
# The first-login Flatpak unit and cooperative Bash editor are both retained.
# The Windows egress reaper gains two path units (tilo-windows-vm-egress-reaper
# .path and tilo-windows-vm-egress-lease.path); they are the intentional delta.
# The user-identity component carries the board-successor verifier, hardware
# profile and pinned Cosign. Its staged-trust and verified-update helpers move
# from direct rootfs COPY to that component at the same installed paths and
# executable mode; the component installer tests cover their staged sources.
# The baked runtime, /usr/share/fonts/tilo and the font-catalogs tree arrive
# in one COPY of the runtime-image-payload tree (hard links carry the font
# catalog's bytes once, #4003): that row replaces the runtime's and the font
# catalogs' native-builder-payload rows; every installed path is unchanged and
# /usr/share/fonts/tilo, formerly written by a RUN, is new to the contract.
# The Plymouth theme arrives rendered from the plymouth-assets stage, which
# replaces the context copy of its sources; make-plymouth-assets.py, which
# rendered it inside the rootfs, no longer enters /usr/local/bin.
# The app store arrives from the app-store stage, the builder's store with the
# env-runtime's host-identical files left out and named in its host-sources;
# the installed path is unchanged.
# tilo-network-start.service replaces tilo-network-start.timer: the network is
# queued once logins are allowed instead of at OnBootSec=0, where it held the
# greeter through network.target.
# The session-priority polkit rule (set-property on session scopes only) ships
# beside the other policies; it is the one payload row added by the interactive
# priority policy. Its unit drop-ins ride the existing /etc/systemd/ COPY.
# Windows compatibility is a system extension (windows-compat-sysext), not
# rootfs content: Proton, the Steam runtime, the virtio-win media, the FEX
# payload and the x86 guest are no longer copied into /usr here.
# The installer locale list is copied from the install-profile-model crate,
# which compiles it in; the installed path is unchanged.
# Prepared migration adds the account capability verifier pair, the home-seed
# and prepared extraction helpers, the prepared-setup component and the selected
# runtime version marker. Atomic firstboot file provisioning adds one helper
# to the existing libexec COPY. All previous payload mappings are retained.
# Home and application capability verification adds four scripts to the
# component-files payload; existing installed mappings remain unchanged.
# The boot audit retains the image signature policy checker beside the audit.
# The compiled ESP writer replaces the context script at the same path and mode.
# Native seed and prepared transport build outputs replace the unshipped Python
# oracles, with executable modes and extension-free runtime paths.
# The atomic firstboot file helper is one intentional payload addition in
# the existing libexec COPY instruction; the layer budget is unchanged.
# The runtime and browser-runtime publisher keys arrive in the
# runtime-image-payload tree, from the same builder result as the runtimes
# they sign, instead of from native-builder-payload: buildkit could serve that
# stage from an earlier invocation's cache while the builder re-ran and signed
# the runtime with a fresh key. The installed paths are unchanged.
# The boot audit's image signature policy checker is one intentional payload
# addition beside the audit in the existing libexec COPY instruction.
# tilo-extension is now compiled: it arrives from the extension component at
# the same path and mode instead of from the context.
# The cache cleanup's policy (/etc/tilo/cache-gc.toml, beside desktop.toml) and
# its system and user tilo-cache-gc.service/.timer units ride existing COPY
# instructions; they are the intentional payload rows.
# The background app update's user tilo-app-update.service/.timer ride the
# existing user-unit COPY instruction; they are the intentional payload rows.
# systemd-bless-boot's drop-in, which lets blessing rename the booted entry on
# the read-only ESP, is one intentional payload row with a COPY of its own: no
# existing instruction copies into that drop-in directory.
# tilo-esp-generator, which mounts the boot loader's ESP at /boot/efi on an
# installed system, is one intentional payload row with a COPY of its own, and
# its helper tilo-esp-mount rides the existing recovery-files libexec COPY.
# The firstboot file helper is now compiled: it arrives from the firstboot-files
# component as /usr/libexec/tilo-firstboot-files, with the mode it had, instead
# of from the context as tilo-firstboot-files.py.
# The board-successor verifier is now compiled: the user-identity component
# installs the tilo-board-successor build output as
# /usr/libexec/tilo-board-successor, with the mode the script had, instead of
# the manifest's tilo-board-successor.py.
# The boot audit's image signature policy checker is now compiled: it arrives
# from the image-policy-check component at the same path and mode instead of
# from the context.
# The dual-boot enlist helper is now compiled. The base image carries it from
# the boot-order component (one new row): it used to reach only the installer
# image, from the installer's overlay of os/usr, and the installer now
# inherits it.
# The staging reclaim helper is now compiled. The base image carries it from
# the staging-reclaim component (one new row): it used to reach only the
# installer image, from the installer's overlay of os/usr, and the installer
# and the targets it installs now inherit it.
# The installer backend's handoff copy, dev SSH and VM export cache helpers are
# now compiled. The base image carries them from the install-helpers component
# (three new rows): they used to reach only the installer image, from the
# installer's overlay of os/usr, and the installer now inherits them.
# The signed offline target's verifier and the USB-free installation guard
# (tilo-staging-handoff) are now compiled too: two more install-helpers rows,
# from the base image instead of the installer's overlay of os/usr.
# The VM install proof emitter is compiled too (one more install-helpers row).
# The gateway's HTTP adapter is installed alongside the socket daemon at
# /usr/libexec/tilo/tilo-model-http (one new model-gateway component row).
# The desktop's wallpapers and their credits arrive from the os-wallpapers
# stage, which fetches the files the tilo-wallpapers catalog pins: two
# intentional payload rows, /usr/share/backgrounds/tilo and
# /usr/share/licenses/tilo-wallpapers, each with a COPY of its own.
# Miracast moved into tilo-projector (the miracast crate), so the Python
# sender's rows are gone: its source tree, its wrapper and the wpa_supplicant
# D-Bus policy only it used.
# Prepared setup and migration runtime artifacts remain alongside the native
# helper components above. xwayland-satellite is built from patched source
# (xwayland-satellite-builder): its binary and license directory are two new
# rows in place of the RPM. The digest covers every expanded manifest output.
# Relay metadata is owned only by its component, not a second rootfs COPY:
# the same six files and 0644 modes are staged for component assembly below.
# The release trust directory (keys/release: the transparency log key, the
# trust epoch and, after the root ceremony, the root certificate) is staged
# with the other trust inputs.
# The flight recorder and tilo-session-report are compiled now (the
# session-record component): their two rows come from desktop-binaries
# instead of COPYs of the Python scripts.
# In-guest test tooling (the AT-SPI client and smokes, the boot, input,
# projection and admin-action checks) no longer ships: os/dev/vm.sh test
# hands it to a booted guest, and the Files contract is bind-mounted.
# tilo-uki-cmdline is tilo-esp-write's link from the esp-write component, not a
# COPY of the Python UKI editor.
# The installed consumer capability producers are tilo-prepared-setup modes
# now, so their six Python script rows are gone.
# hardware_profile.py no longer ships: the image reads its profile with the
# compiled reader, so the component staging row for it is gone.
# verify-no-python-runtime.sh is copied in for the rootfs's final checks.
# The community workspace's vendor default (/usr/share/tilo/community.toml),
# which names the site a computer is in by default, is a deliberate addition.
# The sandbox system view's three units (tilo-sandbox-view.service, .path and
# the retire@ template) are deliberate additions.
# The network-answers helper, search's one door to the network for exchange
# rates, is installed by its net-answers component at
# /usr/libexec/tilo-net-answers: one payload row.
# tilo-runtime-resident.service, which keeps the shared app runtime's
# libraries in memory, is a deliberate addition, and so are its user units
# (tilo-runtime-resident.service and .path), which keep the runtimes a person
# installed into their own store.
# The shell component installs /usr/share/tilo/system-interfaces.toml, the
# interface declaration it reads, so a shell swapped onto an older image never
# judges apps by that image's declaration: its manifest row replaces the
# context COPY of the same file.
# The relay's state migration and its tmpfiles entry (tilo-relay.conf) ship
# with tilo-relay.service, which a home peer enables on this image too.
# bootc's update timer and service each get a drop-in that lets them run on
# the composefs backend, which leaves no /run/ostree-booted: two payload rows.
# The image is built as layer actions (os/image/layers.toml), so its contract is
# every stage of their pieces: the side builds' outputs (bootc, doubletake, joycond,
# xwayland-satellite) arrive through the image-tool-stages image at the same
# destinations, and the seal's UKI row (/boot/EFI/Linux/) is part of it.
# The time-zone polkit rule (49-tilo-time-zone.rules) is a deliberate
# addition beside the other rules, and tilo-identity, the identity command for
# headless devices, is installed by its identity-cli component at
# /usr/libexec/tilo/tilo-identity: one payload row each.
# `tilo device` and `tilo action` are the tilo binary under their own names
# (/usr/bin/device, /usr/bin/action), with completions linked the same way;
# `tilo package` and `tilo agent` are linked the same way, and so is each noun's
# tilo-<noun> name. The programs those nouns drive live in /usr/libexec/tilo/,
# off PATH; each old /usr/bin name is a link to tilo, which runs the program
# under that name: two rows per moved program.
# The public content refresh's user tilo-public-content-refresh.service/.timer
# ride the existing user-unit COPY instruction; they are the intentional
# payload rows.
# NetworkManager's DNS cache (conf.d/25-tilo-dns-cache.conf, dnsmasq caching
# lookups that a lossy Wi-Fi link would otherwise stall for glibc's 5 s
# resolver timeout) is one payload row.
# tilo-deployment-status.socket and its service answer `tilo status` for the
# device admin: two payload rows.
# Signing in adds tilo-authd.service, the tilo-auth-legacy PAM service and the
# sign-in component's tilo-authd, tilo-auth and pam_tilo.so: five payload rows.
# The boot-signin component's tilo-boot-signin is one more.
# tilo-clock-floor, its service, timer and shutdown service keep the clock
# floor PID 1 starts from at chrony's authenticated time: four payload rows
# riding existing COPY instructions.
# The device VPN helper's units (tilo-device-vpn.socket, .service and
# -reset.service) ride the system-unit COPY; its tmpfiles entry, firewalld zone
# and policy, and NetworkManager drop-in (30-tilo-device-vpn.conf) are four
# payload rows (docs/device-vpn.md).
# After the boot loader went back from a deployment that failed its tries,
# tilo-boot-fallback.service settles bootc on the running one and the session's
# tilo-boot-fallback-notice (script and user unit) tells the person: three
# payload rows on the existing recovery-files and user-unit COPY instructions.
expected_contract=75e13497c82537ec28c88f6c7f56ca4b31fb24ce623ad571e6750491ef853ebd
actual_contract="$(containerfile_expanded_rootfs_copy_contract "$containerfile" |
  LC_ALL=C sort | sha256sum | awk '{print $1}')"
[ "$actual_contract" = "$expected_contract" ] || {
  echo "rootfs copy contract changed: expected $expected_contract, got $actual_contract" >&2
  exit 1
}

# A unique destination makes the sorted contract sufficient: no later COPY can
# mask an earlier payload at the same path. Directory-content copies remain
# single-source entries and were intentionally not consolidated.
# Two directory-content copies share /usr/ as their destination, with disjoint
# trees: the Windows provider's (provider.json only; its payload is the
# windows-compat sysext), and the assembled
# runtime tree
# (/usr/lib/tilo-runtime, /usr/share/fonts/tilo, /usr/share/tilo/font-catalogs),
# which must be one COPY so its hard links stay one layer's.
duplicates="$(containerfile_expanded_rootfs_copy_contract "$containerfile" |
  awk -F '\t' '$1 == "runtime-image-payload" && $2 == "/image/usr/" && $3 == "/usr/" { next }
    { count[$3]++ } END { for (path in count) if (count[path] > 1) print path }')"
[ -z "$duplicates" ] || {
  printf 'rootfs copy destinations are no longer unique:\n%s\n' "$duplicates" >&2
  exit 1
}

instructions="$(containerfile_instructions "$containerfile" | awk -v stages=" $(containerfile_image_stages "$containerfile") " '
  $1 == "FROM" { stage=$NF; next }
  stage != "" && index(stages, " " stage " ") && $1 ~ /^(ARG|COPY|RUN|ADD|ENV|LABEL|WORKDIR|USER|EXPOSE|VOLUME|STOPSIGNAL|HEALTHCHECK|SHELL|CMD|ENTRYPOINT)$/ { n++ }
  END { print n+0 }
')"
# The existing image used 123; provider selection adds ARG, COPY and its
# native dynamic-linker validation RUN. Public trust adds four ARGs, two
# additional COPYs and a validation RUN before initramfs generation. External
# AT-SPI validation adds two narrowly scoped COPY instructions.
# Firewall-first NetworkManager ordering adds one explicit drop-in COPY.
# Recording whether the Windows provider is included adds an ARG and a LABEL.
# Synced-worktree git defaults add two: the snippet and the /etc/gitconfig
# that includes it.
# The opt-in extension overlay adds one COPY of its scratch stage.
# The echo-cancel PipeWire config adds one COPY; folding the three
# /usr/lib/systemd/user units into one COPY removes two.
# A build-only resolver placeholder and a final COPY restore the runtime
# symlink outside the cleanup RUN, keeping BuildKit's DNS mount out of /run.
# The fixed-clock build adds an ARG, four COPYs, and an early path assertion.
# Pinning the unpacked base's directory times to the epoch adds one RUN.
# The single virtio Cage wrapper COPY brings the reviewed rootfs to 151.
# The ARM virtual graphics CPU shim adds one COPY, bringing this to 152.
# Redeclaring KERNEL_RPMS_REF before the kernel-swap RUN so Buildah hashes the
# kernel image identity into that layer's cache key adds one ARG (153).
# Redeclaring RPMS_REF before the first snapshot install, for the same reason,
# adds one more (154).
# NetworkManager's Wake-on-LAN drop-in adds one COPY (155).
# The offline predefined Flathub descriptor adds one reviewed COPY (156).
# The image is built as layer actions (os/image/layers.toml): the count is every
# stage of their pieces, the seal's included, 151 of them.
[ "$instructions" -le 156 ] || {
  echo "rootfs layer budget exceeded: $instructions instructions (maximum 156)" >&2
  exit 1
}

# Bulk source-tree overlays are specifically forbidden: they can silently ship
# installer helpers, test fixtures, or future files merely added under os/usr.
if containerfile_instructions "$containerfile" |
   grep -Eq '^COPY( --[^ ]+)* (os/usr/?|os/usr/?) /'; then
  echo 'bulk COPY of os/usr is forbidden' >&2
  exit 1
fi

for overlay in desktop-binaries remote-capture-files rollback-files recovery-files extension-files; do
  [ "$(containerfile_copy_contract "$containerfile" rootfs |
       awk -F '\t' -v s="$overlay" '$1==s && $2=="/" && $3=="/"{n++} END{print n+0}')" -eq 1 ] \
    || { echo "$overlay is not applied to rootfs exactly once" >&2; exit 1; }

  # Scratch overlays may only arrange COPY payload. A RUN could alter hashes,
  # ownership, capabilities, labels, or symlink targets behind the manifest.
  bad_instruction="$(containerfile_instructions "$containerfile" | awk -v wanted="$overlay" '
    $1 == "FROM" { stage=$NF; next }
    stage == wanted && $1 !~ /^(COPY|#)$/ && NF { print; exit }
  ')"
  [ -z "$bad_instruction" ] \
    || { echo "$overlay mutates payload: $bad_instruction" >&2; exit 1; }
done

containerfile_instructions "$containerfile" |
  grep -Eq '^COPY .*--chown(=| )' \
  && { echo 'COPY ownership override requires updating the payload proof' >&2; exit 1; } \
  || true

for path in \
  os/scripts/finalize-rpmdb.sh \
  os/usr/libexec/tilo-disk-encrypt \
  os/usr/libexec/tilo-store-volume \
  os/usr/libexec/tilo-verified-boot-install-preflight \
  os/usr/libexec/tilo-security-audit \
  os/usr/lib/NetworkManager/dispatcher.d/90-tilo-extension; do
  containerfile_instructions "$containerfile" |
    awk -v path="$path" '
      $1 == "COPY" {
        chmod=0; source=0
        for (i=2; i<NF; i++) {
          if ($i == "--chmod=0755") chmod=1
          if ($i == path) source=1
        }
        if (chmod && source) found=1
      }
      END { exit !found }
    ' \
    || { echo "$path lost its enforced 0755 installed mode" >&2; exit 1; }
done

# Component metadata must actually enter the assembly stage: a manifest-only
# dry run cannot prove that the build context supplies the installed bytes.
for path in \
  usr/lib/tilo/node-relay.toml \
  usr/lib/sysusers.d/tilo-relay.conf \
  usr/lib/systemd/system/tilo-node-relay@.service \
  usr/lib/systemd/system/tilo-node-session-key@.service \
  usr/lib/systemd/system/tilo-node-session@.service \
  usr/lib/systemd/system/tilo-node-session@.path; do
  containerfile_copy_contract "$containerfile" component-files |
    awk -F '\t' -v source="os/$path" -v destination="/tilo-components/$path" '
      $1 == "context" && $2 == source && $3 == destination { n++ }
      END { exit n != 1 }' || {
        echo "relay component input is not staged exactly once: $path" >&2
        exit 1
      }
done
python3 "$repo/os/scripts/install-components.py" relay --dry-run --format json |
  python3 -c 'import json,sys
rows = [row for row in json.load(sys.stdin) if (row["from"] or "").startswith("@manifest/")]
assert len(rows) == 6, rows
for row in rows:
    assert row["from"] == "@manifest" + row["to"], row
    assert row["mode"] == "0644" and not row["optional"], row'

echo "rootfs COPY contract unchanged; $instructions-instruction layer budget satisfied"

# The shell's startup companion is now installed by the canonical component
# installer rather than a direct COPY --chmod. Observe that actual image path
# and its mandatory executable-mode plan instead of accepting a staging COPY.
containerfile_installs_output "$containerfile" '@manifest/usr/libexec/tilo-session' /usr/libexec/tilo-session
python3 "$repo/os/scripts/install-components.py" shell --dry-run --format json |
  python3 -c 'import json,sys
rows = [row for row in json.load(sys.stdin) if row["to"] == "/usr/libexec/tilo-session"]
assert len(rows) == 1, "shell must install exactly one startup wrapper"
row = rows[0]
assert row["from"] == "@manifest/usr/libexec/tilo-session", row
assert row["mode"] == "0755" and not row["optional"], row'
