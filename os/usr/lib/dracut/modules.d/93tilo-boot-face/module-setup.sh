#!/usr/bin/bash
# The login screen's face for the boot screen. Plymouth's label plugin draws
# text with /usr/share/fonts/Plymouth.ttf, or Plymouth-monospace.ttf for a
# font name containing "Mono"; the plymouth module links both to the
# image's fontconfig defaults. The Tilo theme carries the login screen's face
# cut to its two weights (os/dev/make-plymouth-assets.py: boot-face.ttf at
# the labels' weight, boot-face-strong.ttf at the names' weight), which the
# plymouth module installs with the theme, and tilo.script asks for the
# strong one with "Mono". This module points both names at them and drops
# the default fonts nothing else in the initramfs reads.

THEME=/usr/share/plymouth/themes/tilo

check() {
    [[ -f "${dracutsysrootdir-}$THEME/boot-face.ttf" ]] || return 1
    [[ -f "${dracutsysrootdir-}$THEME/boot-face-strong.ttf" ]] || return 1
    return 255
}

depends() {
    echo plymouth
    return 0
}

install() {
    local fonts="${initdir:?}/usr/share/fonts" name face old
    mkdir -p "$fonts"
    inst_simple "$THEME/boot-face.ttf"
    inst_simple "$THEME/boot-face-strong.ttf"
    inst_simple "$THEME/glyphs.txt"
    for name in Plymouth.ttf:boot-face.ttf Plymouth-monospace.ttf:boot-face-strong.ttf; do
        face="$THEME/${name#*:}"
        name="$fonts/${name%%:*}"
        old=""
        [[ -L $name ]] && old="$(readlink "$name")"
        ln -sfn "$face" "$name"
        # The default the plymouth module installed, unless the other name
        # still points at it.
        if [[ -n $old && $old != "$THEME"/* ]] \
            && ! [[ $(readlink "$fonts/Plymouth.ttf") == "$old" \
                    || $(readlink "$fonts/Plymouth-monospace.ttf") == "$old" ]]; then
            rm -f "${initdir:?}$old"
        fi
    done
}
