#!/usr/bin/bash
# The greeter as the boot's sign-in screen (docs/security/sign-in.md §3.4).
# tilo-boot-signin starts it in boot mode on the display and input devices it
# opens for it; without it, the sign-in asks on the console through systemd's
# password agents. This module carries the greeter's boot build and what it needs
# to draw, read keyboards and touch, and speak: its faces, the keymaps,
# libinput's device quirks, the touch screen drivers, espeak-ng with the
# voices the screen reader offers, and ALSA with the sound drivers that need
# no large firmware.

GREETER=/usr/libexec/tilo/tilo-greeter-boot
FONTS=/usr/share/fonts/tilo
# The faces the login screen sets its names, labels and keys in: the UI face
# and the Noto faces behind it for the scripts names come in. The CJK face
# (32 MB) and the colour emoji stay out; a name in a script left out shows in
# the next face that has it, or as missing glyphs.
FACES=(
    "TiloSans[wdth,wght].ttf"
    "Onest-Cyrillic[wght].ttf"
    "NotoSans[wght].ttf"
    "NotoSansArabic[wght].ttf"
    "NotoSansHebrew[wght].ttf"
    "NotoSansDevanagari[wght].ttf"
    "NotoSansBengali[wght].ttf"
    "NotoSansThai[wght].ttf"
    "NotoSansSymbols[wght].ttf"
    "NotoSansSymbols2-Regular.ttf"
)
# espeak-ng's dictionaries for the voices Settings offers
# (settings_store::accessibility::ScreenReaderVoice).
VOICES=(en de fr es)

check() {
    require_binaries "$GREETER" || return 1
    return 255
}

depends() {
    return 0
}

# What the initramfs grows by, uncompressed: everything this module puts in
# that no other module already had. os/scripts/build-initramfs.sh holds it to
# a budget.
initramfs_bytes() {
    du -sb --apparent-size "${initdir:?}" | cut -f1
}

install() {
    local face voice data before xkb audio
    before="$(initramfs_bytes)"
    inst_binary "$GREETER" /usr/libexec/tilo/tilo-greeter
    # Not the image's catalog directory: the catalog's manifest lists faces
    # this copy leaves out, so the toolkit takes these as system fonts.
    for face in "${FACES[@]}"; do
        inst_simple "$FONTS/$face" "/usr/share/fonts/tilo-boot/$face"
    done
    # The keymaps live where xkbcommon was built to look (Fedora's
    # xkeyboard-config-2); /usr/share/X11/xkb names them too.
    xkb="$(readlink -f /usr/share/X11/xkb)"
    inst_multiple -o $(find "$xkb" -type f)
    [ "$xkb" = /usr/share/X11/xkb ] || inst_symlink /usr/share/X11/xkb
    inst_multiple -o $(find /usr/share/libinput -type f)
    # The greeter's own words in the image's languages.
    inst_multiple -o $(find /usr/share/tilo/language-packs -type f \
        \( -name greeter.ftl -o -name tilo-ui.ftl \) 2>/dev/null)

    # Without espeak-ng the screen reader has nothing to speak with, and the
    # screen is shown without speech.
    if type -P espeak-ng > /dev/null; then
        inst_binary espeak-ng
        data=/usr/share/espeak-ng-data
        inst_multiple -o "$data/phontab" "$data/phonindex" "$data/phondata" \
            "$data/phondata-manifest" "$data/intonations"
        for voice in "${VOICES[@]}"; do
            inst_simple "$data/${voice}_dict"
        done
        inst_multiple -o $(find "$data/lang" "$data/voices" -type f)
    fi

    inst_libdir_file "libasound.so*"
    # Not alsa.conf.d: the image's drop-ins there send ALSA's default device
    # to PipeWire, which never runs here, and every sound would fail to open.
    # Without them the default is the first card, through dmix or plug.
    inst_multiple -o $(find /usr/share/alsa -type f -not -path '/usr/share/alsa/alsa.conf.d/*')
    # HDA, USB and virtio sound; a machine whose sound needs firmware too
    # large for the initramfs signs in without speech.
    # snd-seq too: Fedora's modprobe.d/dist-alsa.conf loads snd-pcm through
    # an install command that loads snd-seq after it; where snd-seq is
    # missing that command fails, and with it every sound driver on snd-pcm.
    hostonly='' instmods =sound/pci/hda =sound/usb =sound/virtio snd-hda-intel snd-seq || :
    # Touch screens: HID ones come with the kernel-modules module's
    # drivers/hid, the rest are input drivers of their own, and a virtual
    # machine's is a virtio input device.
    hostonly='' instmods =drivers/input/touchscreen virtio_input || :
    # The greeter runs as nobody in the audio group (tilo-boot-signin reads
    # the group's id here), so it may open the sound devices and nothing else.
    # udev gives them to the group only if the initramfs's /etc/group names
    # it. The image has no audio group until systemd-sysusers makes one at
    # boot, so take its id from sysusers.d, or Fedora's 63: it only has to
    # agree with itself here, the root's udev sets the devices up again.
    if ! grep -q '^audio:' "${initdir:?}/etc/group" 2>/dev/null; then
        audio="$(getent group audio | cut -d: -f3)"
        [ -n "$audio" ] || audio="$(sed -n 's/^g[[:space:]]\{1,\}audio[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p' \
            /usr/lib/sysusers.d/*.conf 2>/dev/null | head -n 1)"
        echo "audio:x:${audio:-63}:" >> "${initdir:?}/etc/group"
    fi
    dinfo "tilo-greeter: the greeter adds $(($(initramfs_bytes) - before)) bytes"
}
