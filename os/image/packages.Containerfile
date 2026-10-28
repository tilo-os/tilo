# ---- The packages: the frozen RPM set over the pinned base -----------------
# The first layer action of the OS: everything the package transaction puts over
# the base, keyed by the package lists, the RPM snapshot and the base, so no
# change to Tilo's code or configuration reruns it. The pieces above build over
# it given as an image (os/image/layers.toml).
FROM ${BASE_REF} AS packages
ARG SOURCE_DATE_EPOCH=0

# ---- Directory times independent of the builder ------------------------------
# The base's chunked layers carry entries for few parent directories, so an
# unpacked base has most directory times from when the builder unpacked it. A
# later layer that touches such a directory's metadata copies it up with that
# time, and a time older than SOURCE_DATE_EPOCH escapes the export's clamp: a
# builder that unpacked the base before the commit (a seeded one) produced
# different layers than a fresh one (run 49265). Pin every directory to the
# epoch before anything else runs.
RUN --mount=type=bind,source=os/scripts/pin-directory-times.py,target=/run/tilo-pin-directory-times.py \
    python3 -B /run/tilo-pin-directory-times.py / "${SOURCE_DATE_EPOCH}"

# ---- Finalize SQLite's rpmdb after every rpm transaction --------------------
# rpm's sqlite backend persists `rpmdb.sqlite-wal` and `rpmdb.sqlite-shm` rather
# than letting SQLite delete them when the last connection closes. `-shm` is the
# WAL *index*: pure derived state, valid only for the processes that mapped it,
# and meaningless once the transaction's processes are gone. `-wal` is left at
# zero bytes, i.e. already checkpointed into the database — so neither file
# carries data, and neither survives correctly across a container layer.
#
# Every layer boundary is where that bites. The fedora-bootc base ships neither
# file; every Tilo image built so far shipped both, and every dnf step here left
# a fresh pair in its layer. Reading such a layer back in the following RUN
# yields a structurally corrupt database — duplicate b-tree page references and
# wrong index entry counts under `pragma integrity_check`, with `rpm -qa` and
# `rpm -ql` still fine because the Packages blob is intact while Basenames and
# Dirnames are not. That surfaces as dnf refusing to resolve file dependencies
# the base plainly satisfies ("/usr/bin/basename is needed by distrobox") and
# `rpm -qf` reporting that no package owns files it just installed.
#
# So this is not a workaround for the corruption; it is not shipping derived
# state that was never valid to ship. Only this stage needs it: the builder
# stages are thrown away and nothing in them reads the rpmdb after their one
# transaction, and touching their dnf layers would invalidate their cached
# toolchain for nothing.
#
# What is established, and what is not — because the next person will need to
# know which half to trust. Two earlier claims here were wrong and are corrected
# below; they were drawn from single passes of a fault that does not reproduce
# reliably, which is the trap this note now exists to prevent.
#
#   Established. Carrying the sidecars across a layer boundary does corrupt the
#   database, and dropping them does help. Measured directly, one transaction
#   per arm, everything else identical: sidecars KEPT -> `rpm --verifydb` DIRTY
#   in the next layer and the following dnf transaction fails; sidecars DROPPED
#   -> CLEAN, transaction succeeds. So this line earns its place.
#
#   Also established, and this is the correction. Dropping them is NOT
#   sufficient, and the fault is INTERMITTENT. Replaying one real dnf step onto
#   one real parent layer, with the sidecars dropped every time, gives CLEAN on
#   some runs and DIRTY on others — roughly one in four here. A layer can
#   therefore ship a corrupt rpmdb with no sidecar anywhere in it, which is
#   exactly what happened to the `chrony … glibc-langpack-en … ` step: its layer
#   verified DIRTY while the six dnf layers before it verified CLEAN, and two
#   steps later dnf resolved against the damaged indexes, decided glibc was not
#   installed, and rpm refused the transaction ("package glibc-common … is
#   already installed").
#
#   Ruled out as the mechanism, each by A/B trials rather than reasoning:
#   `sync` before the layer ends (did not help, and the sync arm was dirtier
#   than the plain one); `VACUUM` on the database before dropping the sidecars
#   (rewrites in place, same inode, did not help); and the reproducible-build
#   flags `--source-date-epoch` / `--rewrite-timestamp` (dirty and clean runs on
#   both sides). Nor is the storage stack what the previous note said: podman
#   here is the NATIVE overlay driver on btrfs, not fuse-overlayfs — though
#   "Native Overlay Diff: false" does mean the layer changeset is computed by
#   walking and re-reading the tree, which is where a large file being rewritten
#   in place is most plausibly captured inconsistently.
#
#   NOT established. Why the capture tears. It correlates with how much of the
#   database a transaction rewrites — the step that upgrades glibc, replacing
#   four packages and every file list they own, is the one that fails, while
#   small pure-install steps rarely do — but that is an observation, not a
#   mechanism. CI builds the same Containerfile under buildkit and has never
#   reproduced it.
#
# Rebuild the indexes onto a fresh inode before every boundary, then remove the
# derived sidecars and verify both the database and a base-image file lookup.
# rpm cannot rename its rebuild directory over an overlay-backed /usr/share/rpm,
# so the helper performs a checked single-file replacement within that existing
# directory. This avoids asking the layer diff to capture a heavily rewritten
# database inode and turns corruption into an immediate failure.
COPY --chmod=0755 os/scripts/finalize-rpmdb.sh /usr/libexec/tilo/finalize-rpmdb
COPY --chmod=0755 os/scripts/clean-dnf-layer-state.sh /usr/libexec/tilo/clean-dnf-layer-state
COPY --from=dnf-build-clock /build-clock.so /usr/libexec/tilo/build-clock.so
COPY --chmod=0755 os/scripts/dnf-fixed-time.sh /usr/local/bin/dnf
COPY --chmod=0755 os/scripts/dnf-fixed-time.sh /usr/local/bin/rpm
RUN test "$(command -v dnf)" = /usr/local/bin/dnf \
    && test "$(command -v rpm)" = /usr/local/bin/rpm
ARG FINALIZE_RPMDB="/usr/libexec/tilo/finalize-rpmdb"

# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
ARG TILO_WINDOWS_ARCH
# ---- Every package transaction, in one layer --------------------------------
# These groups used to be a RUN each, and every one ended in FINALIZE_RPMDB,
# which rewrites the whole rpm database at a fresh inode: eighteen layers each
# carried a complete 34-58 MB rpmdb.sqlite, about 830 MB of payload and 250 MB
# of every pull that the installed system never reads. The database
# only has to be finalised where a layer ends, so the groups now run in one RUN,
# in the order they always ran, and FINALIZE_RPMDB runs once at its end. Each
# group keeps its own comment below. None of them is shaped by a Tilo build
# output, so a Rust or /etc change rebuilds none of them; a change to any of
# them reruns all of them, which a change to an early group did anyway (every
# later layer carried that group's database). The order matters: the ukify
# transaction removes packages (openssl, ...) that a later one could otherwise
# require. The Plymouth assets render in their own stage (plymouth-assets), so
# a theme edit does not invalidate this layer.
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo \
    --mount=type=bind,source=os/usr/share/rime-data/default.custom.yaml,target=/run/tilo-rime-default.custom.yaml,ro \
    --mount=type=bind,source=os/usr/share/fontconfig/conf.avail/57-tilo-fonts.conf,target=/tilo-src/57-tilo-fonts.conf,ro \
    test "$(uname -m)" = "$TILO_WINDOWS_ARCH" \
    # Graphics stack. `at-spi2-core` supplies the D-Bus-activated org.a11y.Bus
    # service discovered by AccessKit providers at runtime. `harfbuzz-icu` and
    # `libatomic` own libharfbuzz-icu.so.0 and libatomic.so.1, which the copied
    # shell binary links directly; keep them explicit so compositor startup never
    # depends on build-stage or transitive browser dependencies.
    # `plymouth-plugin-label` draws the boot sign-in screen's names and
    # messages; Plymouth carries only its FreeType half into the initramfs.
    && dnf install -y --setopt=reposdir=/repo \
    mesa-dri-drivers \
    mesa-vulkan-drivers \
    libglvnd-egl \
    harfbuzz-icu \
    libatomic \
    at-spi2-core \
    plymouth \
    plymouth-plugin-label \
    plymouth-plugin-script \
    plymouth-scripts \
    && dnf clean all \
    # Audio/Video
    # alsa-ucm: the per-machine ALSA profiles PipeWire opens DSP sound cards with
    # (Qualcomm X1E, Intel SOF). Without one such a card lists no sink or source
    # (os/tests/board_audio_contract.sh).
    && dnf install -y --setopt=reposdir=/repo \
    alsa-ucm \
    pipewire \
    pipewire-pulseaudio \
    pipewire-utils \
    wireplumber \
    v4l-utils \
    sane-backends \
    sane-backends-drivers-scanners \
    tesseract \
    tesseract-langpack-eng \
    sound-theme-freedesktop \
    && dnf clean all \
    # GStreamer — the Music app plays audio through a `playbin` whose sink chain
    # ends in `autoaudiosink` (→ `pulsesink`, from -plugins-good, over the PipeWire
    # pulse shim above). `gstreamer1-plugin-libav` supplies the AAC/Opus decoders
    # (`avdec_aac`, `avdec_opus`) YouTube-Music streams need — without it the
    # pipeline demuxes but decodes nothing and playback is silent.
    #
    # Video codecs: the image ships only royalty-free software codecs — libvpx
    # (VP9, in -good), libaom and SVT-AV1 (in -bad-free) and dav1d — plus the
    # VA-API/Vulkan hardware paths stock Mesa exposes (AV1/VP9 decode, AV1 encode
    # on AMD). Cisco's OpenH264 binary and its GStreamer plugin are deliberately
    # NOT baked in: Cisco's patent licence covers only binaries the user downloads
    # from Cisco. libopenh264.so.8, which libavcodec-free links, is Fedora's
    # `noopenh264` stub (the frozen snapshot is resolved without Fedora's
    # fedora-cisco-openh264 repository, os/Containerfile.rpms). Cisco's build
    # arrives with the user-downloaded media-codecs extension, whose overlay
    # shadows the stub at the same path. os/scripts/verify-no-cisco-openh264.sh
    # fails the build if either ships.
    #
    # pipewire-gstreamer is pipewiresrc: screen capture reads the PipeWire node
    # the ScreenCast portal grants through it (Miracast, screen sharing).
    #
    # -bad-free's gtkwaylandsink plugin makes GTK 3 a hard dependency (see
    # os/scripts/verify-no-ui-toolkits.sh). Fedora's GTK sound-event helper and
    # desktop-portal backend supplement GTK 3; Tilo plays event sounds with pw-play
    # and serves its own portal backend, so the transactions that could pull them
    # in exclude them, and the toolkit gate fails the build if one still arrives.
    && dnf install -y --setopt=reposdir=/repo \
    --exclude=libcanberra-gtk3 \
    --exclude=xdg-desktop-portal-gtk \
    gstreamer1 \
    gstreamer1-plugins-base \
    gstreamer1-plugins-good \
    gstreamer1-plugins-bad-free \
    gstreamer1-plugin-libav \
    gstreamer1-plugin-dav1d \
    pipewire-gstreamer \
    ffmpeg-free \
    python3 \
    && dnf clean all \
    && gst-inspect-1.0 vp9enc >/dev/null \
    && gst-inspect-1.0 vp9dec >/dev/null \
    && gst-inspect-1.0 av1enc >/dev/null \
    && gst-inspect-1.0 dav1ddec >/dev/null \
    && gst-inspect-1.0 h264parse >/dev/null \
    && gst-inspect-1.0 rtph264pay >/dev/null \
    && gst-inspect-1.0 rtponviftimestamp >/dev/null \
    && gst-inspect-1.0 rtpstreampay >/dev/null \
    # Display manager + fingerprint auth (pam_fprintd for the greeter; the
    # greeter's PAM stack references it via a tolerant `-auth` line).  The same
    # PAM stack unlocks the old GNOME login keyring at password login so
    # tilo-secret-service can import it once; Tilo's own store, sealed under the
    # device identity, serves org.freedesktop.secrets after any login method.
    #
    # `polkit` is required so the greeter's power menu can drive
    # logind: `systemctl suspend` / `reboot` / `poweroff` issue the
    # `org.freedesktop.login1.{suspend,reboot,power-off}` actions, which polkit authorises
    # for the `greetd` user via etc/polkit-1/rules.d/49-tilo-greeter-power.rules.
    # It also ships `polkit-agent-helper-1` (and its activation socket), which the
    # compositor's authentication agent drives to run PAM for administrator
    # prompts from pkexec, GNOME Disks, virt-manager and system Flatpak installs.
    #
    # GNOME Keyring's own D-Bus activation file for org.freedesktop.secrets is
    # not installed at all: tilo-secret-service ships the file at that path
    # (below), and an image file that differs from the package that owns it fails
    # verify-rpm-files.sh. rpm's netshared-path rule skips the one path at
    # install time and records it as not installed, so the package never
    # owns bytes the image replaces.
    && printf '%s\n' '%_netsharedpath /usr/share/dbus-1/services/org.freedesktop.secrets.service' \
        > /etc/rpm/macros.tilo-secret-service \
    && dnf install -y --setopt=reposdir=/repo \
    greetd \
    fprintd \
    fprintd-pam \
    gnome-keyring \
    gnome-keyring-pam \
    polkit \
    && rm -f /etc/rpm/macros.tilo-secret-service \
    && test ! -e /usr/share/dbus-1/services/org.freedesktop.secrets.service \
    && dnf clean all \
    # System utilities. Wi-Fi needs NetworkManager's separately packaged plugin
    # and cfg80211's signed regulatory database; neither is supplied by the base
    # image's NetworkManager package. Keep both in the frozen snapshot roots.
    #
    # `xdg-dbus-proxy` (Fedora package name) is the filtered D-Bus proxy
    # `tilo-launcher` spawns per-sandbox so an app that declares
    # `[sandbox.portals] X = "via-host"` reaches only the portal + fd.o
    # notifications + MPRIS interfaces on the session bus (instead of the
    # full bus surface). Without this package the launcher logs a warning
    # and falls back to a direct bind of the raw session bus — degraded
    # security, no functional regression.
    #
    # The developer Git CLI is intentionally absent from the immutable host. It
    # pulls libcurl/OpenSSL into the trusted OS for a convenience already provided
    # by the sandboxed Git app, which embeds its own Rust-managed libgit2 path.
    # Development shells can install Git inside distrobox without expanding PID 1's
    # image-wide crypto surface.
    #
    # `cups` brings cups-client with it, which is what apps/settings' printers pane
    # actually calls: `lpstat -p -d` for the printer list, `lpstat -o` for the queue
    # and `cancel` per job. The pane already degrades honestly when lpstat is absent,
    # so the image shipped a printers pane that was permanently empty and correct
    # about it. The socket below is what makes those calls reach a daemon.
    #
    # `ntfs-3g` is the userspace NTFS driver behind "Open safely": the NTFS
    # mount service runs it confined, as its own account, for drives the person
    # does not know (crates/system/ntfs-mount). Its probe also checks a volume
    # for hibernation and an unfinished journal before any driver mounts it.
    #
    # NetworkManager is Tilo's sole network and DNS authority. The Fedora bootc
    # base may also carry systemd-networkd and systemd-resolved, adding duplicate
    # network state machines (and a direct libssl consumer) without serving a Tilo
    # path. Remove both subpackages and consume NetworkManager's resolver state.
    # Switch the symlink first: resolved's preun script otherwise tries to migrate
    # live resolver state that does not exist while constructing an offline image.
    # Filter the installed inventory; querying absent names emits diagnostics on
    # stdout, which must never become removal arguments. Removal needs no mirrors.
    # `dnsmasq` is NetworkManager's DNS cache (25-tilo-dns-cache.conf).
    && dnf install -y --setopt=reposdir=/repo \
    bubblewrap \
    flatpak \
    dbus-tools \
    xdg-dbus-proxy \
    distrobox \
    cups \
    cups-ipptool \
    podman \
    foot \
    wl-clipboard \
    NetworkManager \
    NetworkManager-wifi \
    dnsmasq \
    wireless-regdb \
    ModemManager \
    fwupd \
    fuse3 \
    ntfs-3g \
    cage \
    && dnf clean all \
    && ln -sfn ../run/NetworkManager/resolv.conf /etc/resolv.conf \
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && printf '%s\n' "$installed" | awk '/^systemd-(coredump|networkd|resolved)$/' \
           | xargs -r dnf --disablerepo='*' remove -y \
    && test "$(readlink /etc/resolv.conf)" = ../run/NetworkManager/resolv.conf \
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && ! printf '%s\n' "$installed" | grep -Eq '^systemd-(coredump|networkd|resolved)$' \
    && dnf clean all \
    && rm -rf /var/cache/swcatalog \
    # xdg-desktop-portal — the frontend that proxies sandboxed-app
    # capability requests (FileChooser, ScreenCast, Camera) to our
    # Tilo backend (Sandbox Phase 4.3).
    #
    # `dconf` and `gsettings-desktop-schemas` let unsandboxed GTK/GNOME host apps
    # read the OS appearance the compositor mirrors into
    # org.gnome.desktop.interface; without the dconf backend GSettings falls back
    # to an in-memory store and every host app keeps its own defaults.
    # `xdg-utils` gives host apps `xdg-open`/`xdg-mime`, which read the
    # tilo-mimeapps.list the compositor publishes; `xdg-user-dirs` names the
    # Documents/Downloads/… folders host apps expect. `geoclue2` is the service
    # xdg-desktop-portal's Location portal asks; without it every request fails.
    # Fcitx5 is the composing input method the compositor starts when a Rime
    # (Simplified Chinese Pinyin), Anthy or Hangul input source is active; it
    # reaches apps through input-method-v2 on a private display, so neither its
    # toolkit IM modules nor its Qt configuration tool (the session writes its
    # configuration) are wanted. These engines link no UI toolkit; Chinese Addons'
    # Pinyin would bring Qt6 and QtWebEngine for its dictionary tools. Rime's
    # shared configuration makes Simplified Chinese Pinyin its only schema.
    && dnf install -y --setopt=reposdir=/repo \
    --exclude=fcitx5-configtool \
    --exclude=fcitx5-gtk \
    --exclude=fcitx5-qt \
    --exclude=fcitx5-qt-module \
    --exclude=libcanberra-gtk3 \
    --exclude=xdg-desktop-portal-gtk \
    xdg-desktop-portal \
    dconf \
    gsettings-desktop-schemas \
    xdg-utils \
    xdg-user-dirs \
    geoclue2 \
    fcitx5 \
    fcitx5-rime \
    fcitx5-anthy \
    fcitx5-hangul \
    && dnf clean all \
    && install -D -m 0644 /run/tilo-rime-default.custom.yaml /usr/share/rime-data/default.custom.yaml \
    # -------------------------------------------------------------------------
    # OS infrastructure (C2-C8)
    # -------------------------------------------------------------------------
    # C2 time sync (chrony), C3 zram swap (zram-generator), C4 locale data,
    # C5 firewall (firewalld), C7 bluetooth (bluez), C8 power (power-profiles-daemon).
    # Audio (C6) reuses the pipewire stack installed above.
    # The Imager exposes a multilingual locale catalog. Ship every corresponding
    # glibc language pack so each accepted profile remains functional after first
    # boot; advertising languages backed by only glibc-langpack-en would silently
    # apply an unusable LANG value and fall back to English. Keep this curated set
    # in lock-step with crates/session/install-profile-model/locales.txt rather than pulling
    # the much larger all-locales archive into every download. libevdev is the
    # runtime library of joycond (see joycond-builder).
    && dnf install -y --setopt=reposdir=/repo \
    chrony \
    zram-generator \
    glibc-langpack-ar \
    glibc-langpack-cs \
    glibc-langpack-da \
    glibc-langpack-de \
    glibc-langpack-en \
    glibc-langpack-es \
    glibc-langpack-fi \
    glibc-langpack-fr \
    glibc-langpack-it \
    glibc-langpack-ja \
    glibc-langpack-ko \
    glibc-langpack-nb \
    glibc-langpack-nl \
    glibc-langpack-pl \
    glibc-langpack-pt \
    glibc-langpack-ru \
    glibc-langpack-sv \
    glibc-langpack-tr \
    glibc-langpack-uk \
    glibc-langpack-zh \
    firewalld \
    bluez \
    libevdev \
    power-profiles-daemon \
    && dnf clean all \
    # Offline spelling dictionaries used by the shared text toolkit, which reads
    # /usr/share/hunspell in-process (also inside app sandboxes); English also has
    # an embedded word list so recovery/minimal environments retain checking. The
    # `hunspell` CLI is for people at a terminal. Keep this explicit rather
    # than pulling every Fedora language pack into the immutable image.
    && dnf install -y --setopt=reposdir=/repo \
    hunspell \
    hunspell-en \
    hunspell-de \
    hunspell-es \
    hunspell-fr \
    hunspell-it \
    hunspell-nl \
    hunspell-pl \
    hunspell-pt \
    hunspell-ru \
    && dnf clean all \
    # C9 printing (CUPS plus its standard IPP query tool). Deliberately NOT
    # `cups-browsed`, which is
    # the daemon that turns *remote* DNS-SD advertisements into local print
    # queues and was the 2024 RCE chain (CVE-2024-47076/47175/47176/47177). Its
    # absence is why nothing on the network can make a print queue appear here.
    # Confinement lives in os/usr/lib/systemd/system/cups.service.d/ and
    # os/etc/cups/cupsd.conf; see docs/printing-threat-model.md.
    && dnf install -y --setopt=reposdir=/repo \
    cups \
    cups-ipptool \
    && dnf clean all \
    # Ensure hardware-access groups are materialised in the deployment's /etc and
    # add the greeter to them. `getent` is deliberately not used here: Fedora's NSS
    # stack can synthesize these well-known groups from systemd's user database
    # even when /etc/group has no corresponding record. That made this build step
    # incorrectly skip the records, after which first-boot `useradd -G video`
    # failed and left a freshly installed machine without its configured account.
    && { grep -q '^video:' /etc/group || echo "video:x:39:greetd" >> /etc/group; } \
    && { grep -q '^input:' /etc/group || echo "input:x:104:greetd" >> /etc/group; } \
    && sed -i 's/^video:x:39:$/video:x:39:greetd/' /etc/group \
    && sed -i 's/^input:x:104:$/input:x:104:greetd/' /etc/group \
    && grep -q '^video:x:39:.*greetd' /etc/group \
    && grep -q '^input:x:104:.*greetd' /etc/group \
    # Fonts. Tilo's pinned font catalog is the system's one copy of its Noto
    # faces, found by fontconfig, the UI toolkit and the PDF helper (which reads
    # it through a read-only Landlock grant) alike. Its files arrive at
    # /usr/share/fonts/tilo with the runtime bundle (runtime-image-payload), as hard
    # links to the runtime's own copy, so the bytes ship once; the rootfs checks
    # fontconfig resolves them after that copy. Fedora's static Noto packages
    # would ship the same families a second time, several times larger, so the
    # snippet installed here gives the catalog their generic-family defaults.
    # Adwaita Sans is not a product font of the catalog, so it stays a package.
    && dnf install -y --setopt=reposdir=/repo \
        fontconfig \
        adwaita-sans-fonts \
    && dnf clean all \
    && install -D -m 0644 /tilo-src/57-tilo-fonts.conf /usr/share/fontconfig/conf.avail/57-tilo-fonts.conf \
    && ln -s ../../../usr/share/fontconfig/conf.avail/57-tilo-fonts.conf /etc/fonts/conf.d/57-tilo-fonts.conf \
    && fc-cache -s \
    # The frozen snapshot already resolves these development specs and their exact
    # runtime library closure. Erase the header/pkg-config payloads in the same
    # layer; their runtime dependencies remain for the copied compositor binaries.
    # The base already ships pkgconf, so the snapshot would not carry it; a later
    # stage (Wine's mingw64-libxslt) needs it again after this erase.
    # tilo-rpm-snapshot-package: pkgconf
    # tilo-rpm-snapshot-package: pkgconf-m4
    # tilo-rpm-snapshot-package: libpkgconf
    && dnf install -y --setopt=reposdir=/repo \
    wayland-devel \
    libxkbcommon-devel \
    libinput-devel \
    && rpm -e --nodeps wayland-devel libxkbcommon-devel libinput-devel \
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && printf '%s\n' "$installed" | awk '/^(pkgconf-pkg-config|pkgconf|pkgconf-m4|libpkgconf)$/' \
           | xargs -r rpm -e --nodeps \
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && ! printf '%s\n' "$installed" | grep -Eq '^(wayland-devel|libxkbcommon-devel|libinput-devel|pkgconf-pkg-config|pkgconf|pkgconf-m4|libpkgconf)$' \
    && dnf clean all \
    # -------------------------------------------------------------------------
    # Packages that no Tilo build output shapes
    # -------------------------------------------------------------------------
    # Every transaction below, and the SELinux policy load that follows them, reads
    # only the frozen snapshot and a few configuration files. They
    # come before the first Tilo binary is copied in, so a Rust or /etc change
    # rebuilds none of them; they used to follow it, and every such change reran
    # the Windows packages, the bootloader swap and the policy load. Their order
    # among themselves is unchanged: the ukify transaction removes packages (openssl,
    # ...) that a later one could otherwise require.
    #
    # ---- Browser engine runtime libraries -----------------------------------
    # The shared libraries CEF's libcef.so records in DT_NEEDED that nothing else in
    # this image brings. The engine runtime bundle carries CEF itself; these come
    # from the image, which the browser's sandbox reads (/usr/lib64). The rootfs
    # check below fails the build, naming the library, when the list falls short.
    && dnf install -y --setopt=reposdir=/repo \
    nss \
    nspr \
    atk \
    at-spi2-atk \
    cups-libs \
    libXcomposite \
    libXdamage \
    libXrandr \
    libXext \
    libXfixes \
    libxkbcommon \
    mesa-libgbm \
    pango \
    cairo \
    alsa-lib \
    dbus-libs \
    && ldconfig \
    && dnf clean all \
    && gst-inspect-1.0 h264parse >/dev/null \
    && gst-inspect-1.0 hlssink2 >/dev/null \
    && gst-inspect-1.0 pulsesrc >/dev/null \
    && gst-inspect-1.0 avenc_aac >/dev/null \
    && gst-inspect-1.0 aacparse >/dev/null \
    # Miracast (tilo-projector): the ScreenCast portal's PipeWire node, muxed
    # to MPEG-TS and sent as RTP.
    && gst-inspect-1.0 pipewiresrc >/dev/null \
    && gst-inspect-1.0 mpegtsmux >/dev/null \
    && gst-inspect-1.0 rtpmp2tpay >/dev/null \
    # -------------------------------------------------------------------------
    # Bootloader — swap GRUB for systemd-boot
    # -------------------------------------------------------------------------
    # systemd-boot is smaller + simpler than GRUB (no menu render step, no module
    # load path, BLS entries loaded straight off the ESP), so cold boot to the
    # kernel handoff is measurably shorter. bootc chooses which bootloader to
    # plant on the ESP at install time based on the `bootloader` key in the
    # /usr/lib/bootc/install/*.toml drop-ins baked into this image (see the
    # 40-tilo.toml file below). The `systemd-boot-unsigned` package supplies the
    # EFI binaries + BLS layout that bootc/bootupd copy across.
    #
    # The GRUB stack (grub2 + shim + bootupd's grub2 side) is pulled in by the
    # fedora-bootc base and would otherwise still ship on the rootfs, wasting
    # ~30 MB and leaving dead code paths in bootupd. Drop it after installing
    # systemd-boot so nothing during install can pick GRUB by accident.
    #
    # dnf5 refuses to remove grub2-efi-x64 / grub2-pc / grub2-tools-minimal /
    # shim-x64 (each ships an `/etc/dnf/protected.d/*.conf` marking itself
    # protected against removal) and won't accept `--allowerasing` on `remove`.
    # The systemd-boot install already satisfies the "some bootloader must
    # exist" invariant, so shell out to `rpm -e --nodeps` for the protected
    # ones — dnf handles the unprotected ones on the same pass.
    #
    # Tilo's narrow Rust crypto tool replaces the trust that removal takes away.
    # Deleting shim and GRUB, and building a custom kernel, leaves nothing on the boot
    # path that a distribution key covers — so the machine has to be able to sign its
    # own artefacts and enrol its own Platform Key. tilo-crypto does the signing,
    # verification, and bounded efivarfs enrollment. See docs/security/boot-chain.md.
    # bootupd goes with them, and that is a requirement of the sealed layout rather
    # than tidiness: bootc refuses to seal an image that still offers a bootupd
    # install path, because bootupd only ever plants GRUB. Its payload directory and
    # the legacy ostree boot dir have to go too — bootc finds the path through either,
    # and so does the base's preset link to bootupd's bootloader-update.service, which
    # would otherwise point at nothing. os-prober is GRUB's helper for listing other
    # systems in its menu; without GRUB nothing runs it, and it requires grub2-tools.
    # Nothing on this image needs it: with a UKI there is exactly one PE binary on the
    # boot path, /usr/libexec/tilo-secureboot signs it with tilo-crypto, and
    # tilo-secureboot-sign.path already watches /boot/EFI/Linux for a newly staged one.
    && dnf install -y --setopt=reposdir=/repo \
        systemd-boot-unsigned \
    && rpm -qa | grep -E '^(shim-(x64|aa64)|grub2-|os-prober-)' | xargs -r rpm -e --nodeps \
    && rpm -e --nodeps bootupd \
    && rm -rf /usr/lib/bootupd /usr/lib/ostree-boot \
    && rm -f /etc/systemd/system/multi-user.target.wants/bootloader-update.service \
    && dnf clean all \
    # The fedora-bootc base ships the NFS client and server stack. The Tilo kernel
    # has no NFS or rpc_pipefs (os/kernel/config-fragments/tilo.config: remote
    # storage is the userspace QuickFS protocol), so its units could only fail at
    # every boot (var-lib-nfs-rpc_pipefs.mount: "unknown filesystem type
    # 'rpc_pipefs'"). Remove the packages rather than their units; rpm -e without
    # --nodeps fails the build if anything else comes to need them.
    #
    # The base also ships the AWS SDK for Python: sos (the support-report tool)
    # only *recommends* python3-boto3, for uploading a report to S3, and boto3
    # brings s3transfer, jmespath and python3-botocore, whose bundled API data is
    # 123 MB of the root (#4003; `tools/perf/image_size.py --why
    # python3-botocore` traces the chain). sos stays; nothing requires the SDK.
    # This shrinks the installed root; the pull keeps the bytes, which the base's
    # layer and its undeployed /sysroot ostree repository still carry.
    && rpm -e nfs-utils nfs-client-utils nfsv3-client-utils nfsv4-client-utils \
        nfs-common-utils rpcbind \
        python3-boto3 python3-s3transfer python3-botocore python3-jmespath \
    # The ukify transaction. The Plymouth splash once rendered here, with the
    # Pillow ukify brings; it renders in plymouth-assets now. The transaction
    # stays as it was, with what it removes: openssl, the Secure Boot and Git
    # tools and the SSH server leave the image here.
    # Retain the base's openssh and openssh-clients: virt-viewer requires them later,
    # and the frozen RPM transaction assumes dependencies already in the base remain
    # installed. The SSH server remains forbidden by the runtime crypto policy.
    && dnf install -y --setopt=reposdir=/repo \
        systemd-ukify \
    && dnf --disablerepo='*' remove -y systemd-ukify python3-pillow \
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && printf '%s\n' "$installed" | awk '/^(systemd-ukify|openssl|sbsigntools|efitools|mokutil|pesign|nss-tools|gnutls-utils|git|git-core|openvpn|NetworkManager-openvpn|openssh-server)$/' \
           | xargs -r dnf --disablerepo='*' remove -y \
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && ! printf '%s\n' "$installed" | grep -Eq '^(python3-pillow|systemd-ukify|openssl|sbsigntools|efitools|mokutil|pesign|nss-tools|gnutls-utils|git|git-core|openvpn|NetworkManager-openvpn|openssh-server)$' \
    && rpm -q openssh openssh-clients \
    && dnf clean all \
    # tpm2-tools: the NV counter, the policy sessions and TPM2_NV_Certify are driven
    # through it. Also reachable from recovery mode, which is where someone will need
    # it when automatic unlock has failed.
    && dnf install -y --setopt=reposdir=/repo \
        tpm2-tools \
    && dnf clean all && rm -rf /var/cache/dnf /var/log/dnf* /var/lib/dnf \
    # Hardened userspace allocator (scudo, from compiler-rt) LD_PRELOADed into the
    # trusted-base daemon fs.service. Resolve the lib path at build time and bake a
    # drop-in, so a packaging move can't silently leave a dangling LD_PRELOAD that
    # would fail the service. (Base daemons use the system allocator — verified no
    # jemalloc — so the preload is safe.)
    #
    # Alongside it, hardware shadow stacks. Every binary in this image is marked
    # shadow-stack compatible at link time, but Fedora's glibc only switches them on
    # when a process also asks through this tunable — which is precisely what makes
    # the protection scopeable instead of global. fs.service earns it because it
    # parses bytes that arrived from a peer. Measured cost on an allocation-heavy
    # loop: about 8%. If the CPU, the kernel or any library loaded at startup
    # cannot support it, glibc leaves shadow stacks off rather than failing, and
    # tilo-security-audit reports which processes actually got them. A library
    # dlopened later is different: glibc refuses one without the SHSTK note (see
    # crates/packaging/launcher/src/hardening.rs for the libzstd/Mesa case).
    #
    # Only Scudo has to remain for tilo-launcher's scoped hardening. Copy that one
    # DSO to a stable image-owned path, then erase compiler-rt's other sanitizers,
    # runtimes, headers, and versioned toolchain tree.
    && dnf install -y --setopt=reposdir=/repo compiler-rt && \
    scudo="$(find /usr/lib /usr/lib64 -name 'libclang_rt.scudo_standalone.so' 2>/dev/null | head -1)" && \
    test -n "$scudo" && \
    install -D -m 0755 "$scudo" /usr/lib/tilo/libclang_rt.scudo_standalone.so && \
    rpm -e --nodeps compiler-rt clang-resource-filesystem && \
    ! rpm -q compiler-rt clang-resource-filesystem >/dev/null 2>&1 && \
    test -s /usr/lib/tilo/libclang_rt.scudo_standalone.so && \
    dnf clean all && \
    scudo=/usr/lib/tilo/libclang_rt.scudo_standalone.so && \
    mkdir -p /usr/lib/systemd/system/fs.service.d && \
    printf '[Service]\n# Hardened userspace allocator; path resolved at image build.\nEnvironment=LD_PRELOAD=%s\n# Hardware shadow stacks. The ELF marking alone does not enable them.\nEnvironment=GLIBC_TUNABLES=glibc.cpu.hwcaps=SHSTK\n' "$scudo" \
      > /usr/lib/systemd/system/fs.service.d/10-hardened-malloc.conf && \
    echo "scudo wired: $scudo" \
    # Windows compatibility packages. They are installed here with the other
    # packages; the Windows provider, the compacted VirtIO media and the runners
    # are copied at the end of the rootfs, because rootless Podman rescans a layer
    # that large for every later COPY/RUN.
    # Wine is the tested desktop-program baseline; UMU + its pinned Proton build are
    # copied at the end for non-Steam games. The appliance packages provide one
    # KVM-accelerated, TPM-backed Windows fallback. Changing this list requires
    # refreshing RPMS_REF (see the file header).
    # tilo-rpm-snapshot-package[x86_64]: edk2-ovmf
    # tilo-rpm-snapshot-package[x86_64]: wine
    # tilo-rpm-snapshot-package[x86_64]: wine-dxvk
    # tilo-rpm-snapshot-package[aarch64]: edk2-aarch64
    # Native FEX executable dependencies and host graphics/audio thunks.
    # tilo-rpm-snapshot-package[aarch64]: glibc
    # tilo-rpm-snapshot-package[aarch64]: libgcc
    # tilo-rpm-snapshot-package[aarch64]: libstdc++
    # tilo-rpm-snapshot-package[aarch64]: libglvnd-glx
    # tilo-rpm-snapshot-package[aarch64]: libglvnd-opengl
    # tilo-rpm-snapshot-package[aarch64]: libglvnd-egl
    # tilo-rpm-snapshot-package[aarch64]: alsa-lib
    # tilo-rpm-snapshot-package[aarch64]: libdrm
    # tilo-rpm-snapshot-package[aarch64]: libwayland-client
    # tilo-rpm-snapshot-package[aarch64]: vulkan-loader
    # wine only recommends dosbox-staging, a DOS emulator no Tilo Windows path
    # uses. It would bring opusfile, whose HTTP stream reader links OpenSSL.
    # QEMU is the core emulator plus the SPICE and virtio display modules the
    # appliance's domain uses, and libvirt the KVM daemon set without the
    # `qemu-kvm`/`libvirt-daemon-kvm` metapackages: those add QEMU's GTK and SDL
    # front ends and network block drivers no Tilo path opens.
    # libvirt's storage drivers recommend the NFS client stack, which would undo the
    # removal above: the kernel has no NFS, so its units only fail at boot.
    && case "$(uname -m)" in \
      x86_64) vm_firmware=edk2-ovmf; windows_packages='wine wine-dxvk' ;; \
      aarch64) vm_firmware=edk2-aarch64; windows_packages='glibc libgcc libstdc++ libglvnd-glx libglvnd-opengl libglvnd-egl alsa-lib libdrm libwayland-client vulkan-loader' ;; \
      *) echo 'unsupported Windows VM firmware architecture' >&2; exit 1 ;; \
    esac && dnf install -y --setopt=reposdir=/repo \
    --exclude=dosbox-staging \
    --exclude=libcanberra-gtk3 \
    --exclude=xdg-desktop-portal-gtk \
    --exclude=nfs-utils --exclude=nfs-client-utils --exclude=nfs-common-utils \
    --exclude=nfsv3-client-utils --exclude=nfsv4-client-utils --exclude=rpcbind \
    $windows_packages \
    qemu-kvm-core \
    qemu-ui-spice-core \
    qemu-char-spice \
    qemu-audio-spice \
    qemu-device-display-virtio-gpu \
    qemu-device-display-virtio-gpu-pci \
    qemu-device-display-virtio-vga \
    libvirt-client \
    libvirt-daemon-common \
    libvirt-daemon-driver-interface \
    libvirt-daemon-driver-network \
    libvirt-daemon-driver-nodedev \
    libvirt-daemon-driver-nwfilter \
    libvirt-daemon-driver-qemu \
    libvirt-daemon-driver-secret \
    libvirt-daemon-driver-storage \
    libvirt-daemon-lock \
    libvirt-daemon-log \
    libvirt-daemon-plugin-lockd \
    libvirt-daemon-proxy \
    libvirt-daemon-config-nwfilter \
    swtpm \
    swtpm-tools \
    "$vm_firmware" \
    freerdp \
    xorg-x11-server-Xwayland \
    xcb-util-cursor \
    open-sans-fonts \
    waypipe \
    virtiofsd \
    vhost-device-vsock \
    skopeo \
    virt-viewer \
    xorriso \
    genisoimage \
    udftools \
    # The compositor's development specs and Wine's mingw toolchain bring Ninja
    # and CMake's RPM macros. Neither is a runtime dependency, and both need
    # Python, so they go once the last transaction above has run.
    && installed="$(rpm -qa --qf '%{NAME}\n')" \
    && printf '%s\n' "$installed" | grep -Ex 'cmake-rpm-macros|ninja-build' \
           | xargs -r rpm -e --nodeps \
    && ! rpm -qa --qf '%{NAME}\n' | grep -Eq '^(cmake-rpm-macros|ninja-build)$' \
    && dnf clean all && ${FINALIZE_RPMDB}

# Flatpak applies predefined remotes to the default system installation offline,
# importing their embedded GPG key. The root initializer completes this before
# login, so the first Store discovery sees defaults instead of racing lazy
# initialization. Flatpak's applied-remotes history preserves disabled/removed
# defaults; the initializer never uses unconditional remote-add.
