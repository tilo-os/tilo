#!/usr/bin/bash
# Finding and opening the root: tilo-root-discover, and tilo-auth, the one
# program that opens the system volume in early boot. Every way in is here:
# a person's sign-in secret or recovery key, the TPM for a signed boot chain
# (tilo-rollback's unlock helper), and another of the owner's devices
# releasing its half, for which the image's NetworkManager reaches them. The
# network stays idle unless the root's LUKS2 header carries a
# tilo-sibling-unlock token, so an ordinary boot starts nothing for it.

check() {
    require_binaries /usr/libexec/tilo/tilo-rollback cryptsetup systemd-ask-password systemd-escape \
        systemctl NetworkManager ip || return 1
    return 255
}

depends() {
    # dbus: NetworkManager asks wpa_supplicant for Wi-Fi over the system bus.
    echo "systemd-cryptsetup systemd-ask-password tpm2-tss dbus"
    return 0
}

install() {
    inst_multiple awk bash cat chmod cryptsetup dd find grep install ln lsblk \
        readlink rm seq sleep systemd-ask-password systemd-cryptsetup systemd-escape \
        tr udevadm \
        tpm2_createprimary tpm2_flushcontext tpm2_getcap tpm2_getpolicydigest \
        tpm2_load tpm2_loadexternal tpm2_nvread tpm2_nvreadpublic tpm2_pcrread \
        tpm2_policyauthorize tpm2_policyauthvalue tpm2_policynv \
        tpm2_policypcr tpm2_policyor tpm2_quote tpm2_readpublic tpm2_startauthsession \
        tpm2_unseal tpm2_verifysignature \
        systemctl systemd-creds ip NetworkManager
    # shellcheck disable=SC2154 # dracut defines moddir before loading modules
    inst_hook cmdline 90 "$moddir/parse-tilo-root.sh"
    # The initramfs keeps the old name on its PATH, for the unlock helper;
    # the image's /usr/bin/tilo-rollback is a link to `tilo`, which it lacks.
    inst_simple /usr/libexec/tilo/tilo-rollback /usr/bin/tilo-rollback
    # The sign-in screen tilo-root-discover shows for a root people sign in
    # to, and tilo-auth, which opens the volume. tilo-auth reads the header's
    # tokens with cryptsetup; it opens a person's sealed key with
    # tpm2_createprimary, tpm2_load, tpm2_startauthsession (the encrypted
    # session), tpm2_unseal and tpm2_flushcontext, and tpm2_getcap reports
    # the tries left; a machine enrolled to unlock unattended quotes its boot
    # chain with tpm2_quote and tpm2_pcrread; the sibling material unseals
    # with systemd-creds. All are in the list above. check() deliberately does
    # not require tilo-boot-signin or tilo-auth: a root nobody signs in to
    # still needs this module, and the screen steps aside whenever it finds
    # nobody to sign in.
    inst_simple /usr/libexec/tilo/tilo-boot-signin
    inst_simple /usr/libexec/tilo/tilo-auth /usr/bin/tilo-auth
    # The network the sibling unlock reaches the owner's other devices over:
    # Wi-Fi, for a machine whose owner chose a Wi-Fi network for unlocking.
    inst_multiple -o wpa_supplicant
    inst_libdir_dir "NetworkManager/$(NetworkManager --version)"
    inst_libdir_file -o "NetworkManager/$(NetworkManager --version)/libnm-device-plugin-wifi.so"
    # shellcheck disable=SC2154 # dracut defines dbussystem and systemdsystemunitdir
    inst_multiple -o \
        "$dbussystem"/org.freedesktop.NetworkManager.conf \
        "$dbussystem"/wpa_supplicant.conf \
        /usr/share/dbus-1/system-services/fi.w1.wpa_supplicant1.service \
        "$systemdsystemunitdir"/wpa_supplicant.service
    # The internal DHCP client keys its client id on the machine id.
    [ -e "$initdir/etc/machine-id" ] || : > "$initdir/etc/machine-id"
    inst_simple "$moddir/tilo-unlock-network.conf" /etc/NetworkManager/tilo-unlock-network.conf
    inst_simple "$moddir/tilo-unlock-network.service" "$systemdsystemunitdir/tilo-unlock-network.service"
    # The sibling attempt, which tilo-root-discover starts beside the prompt.
    inst_simple "$moddir/tilo-unlock-sibling@.service" "$systemdsystemunitdir/tilo-unlock-sibling@.service"
    inst_simple /usr/libexec/tilo-rollback-initrd-unlock
    inst_simple /usr/libexec/tilo-root-discover
    inst_simple /usr/lib/systemd/system-generators/tilo-root-generator
    inst_simple /usr/share/tilo/tpm2-pcr-signing.public.pem
}
