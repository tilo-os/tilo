#!/usr/bin/bash

check() {
    require_binaries /usr/libexec/tilo/tilo-rollback cryptsetup systemd-ask-password systemd-escape || return 1
    return 255
}

depends() {
    echo "systemd-cryptsetup systemd-ask-password tpm2-tss"
    return 0
}

install() {
    inst_multiple awk bash cat chmod cryptsetup dd find grep install ln lsblk \
        readlink rm seq sleep systemd-ask-password systemd-cryptsetup systemd-escape \
        tr udevadm \
        tpm2_createprimary tpm2_flushcontext tpm2_getcap tpm2_getpolicydigest \
        tpm2_load tpm2_loadexternal tpm2_nvread tpm2_nvreadpublic \
        tpm2_policyauthorize tpm2_policyauthvalue tpm2_policynv \
        tpm2_policypcr tpm2_policyor tpm2_readpublic tpm2_startauthsession \
        tpm2_unseal tpm2_verifysignature
    # shellcheck disable=SC2154 # dracut defines moddir before loading modules
    inst_hook cmdline 90 "$moddir/parse-tilo-root.sh"
    # The initramfs keeps the old name on its PATH, for the unlock helper;
    # the image's /usr/bin/tilo-rollback is a link to `tilo`, which it lacks.
    inst_simple /usr/libexec/tilo/tilo-rollback /usr/bin/tilo-rollback
    # The sign-in screen tilo-root-discover shows for a root people sign in
    # to, and tilo-auth, which checks their secret and derives the volume's
    # key. tilo-auth reads the sign-in tokens with cryptsetup and opens a
    # person's sealed key with tpm2_createprimary, tpm2_load,
    # tpm2_startauthsession (the encrypted session), tpm2_unseal and
    # tpm2_flushcontext; tpm2_getcap reports the tries left. All are in
    # the list above. check() deliberately does not require either program:
    # a root nobody signs in to still needs this module, and the screen
    # steps aside whenever it finds nobody to sign in.
    inst_simple /usr/libexec/tilo/tilo-boot-signin
    inst_simple /usr/libexec/tilo/tilo-auth /usr/bin/tilo-auth
    inst_simple /usr/libexec/tilo-rollback-initrd-unlock
    inst_simple /usr/libexec/tilo-root-discover
    inst_simple /usr/lib/systemd/system-generators/tilo-root-generator
    inst_simple /usr/share/tilo/tpm2-pcr-signing.public.pem
}
