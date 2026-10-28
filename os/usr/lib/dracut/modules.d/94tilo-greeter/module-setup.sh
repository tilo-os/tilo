#!/usr/bin/bash
# The greeter as the boot's sign-in screen (docs/security/sign-in.md §3.4).
# tilo-boot-signin starts it in boot mode on the display and input devices it
# opens for it; without it, the sign-in asks through Plymouth or the console
# as before. This module carries the greeter's boot build and what it needs
# to draw, read keyboards and touch, and speak: its faces, the keymaps,
# libinput's device quirks, espeak-ng with the voices the screen reader
# offers, and ALSA with the sound drivers that need no large firmware.

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
    local face voice data before xkb
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
    inst_multiple -o $(find /usr/share/alsa -type f)
    # HDA, USB and virtio sound; a machine whose sound needs firmware too
    # large for the initramfs signs in without speech.
    hostonly='' instmods =sound/pci/hda =sound/usb =sound/virtio snd-hda-intel || :
    # The greeter runs as nobody in the audio group (tilo-boot-signin reads
    # the group's id here), so it may open the sound devices and nothing else.
    grep -q '^audio:' "${initdir:?}/etc/group" 2>/dev/null \
        || grep '^audio:' /etc/group >> "${initdir:?}/etc/group"
    dinfo "tilo-greeter: the greeter adds $(($(initramfs_bytes) - before)) bytes"
}
