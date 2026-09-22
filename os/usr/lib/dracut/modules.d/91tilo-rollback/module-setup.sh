#!/usr/bin/bash

check() {
    require_binaries tilo-rollback cryptsetup systemd-ask-password systemd-escape || return 1
    return 255
}

depends() {
    echo "systemd-cryptsetup systemd-ask-password tpm2-tss"
    return 0
}

install() {
    inst_multiple awk bash cat chmod cryptsetup dd find grep install ln lsblk \
        readlink rm seq sleep systemd-ask-password systemd-cryptsetup systemd-escape \
        tilo-rollback tr udevadm \
        tpm2_createprimary tpm2_flushcontext tpm2_getpolicydigest \
        tpm2_load tpm2_loadexternal tpm2_nvread tpm2_nvreadpublic \
        tpm2_policyauthorize tpm2_policyauthvalue tpm2_policynv \
        tpm2_policypcr tpm2_policyor tpm2_readpublic tpm2_startauthsession \
        tpm2_unseal tpm2_verifysignature
    # shellcheck disable=SC2154 # dracut defines moddir before loading modules
    inst_hook cmdline 90 "$moddir/parse-tilo-root.sh"
    inst_simple /usr/bin/tilo-rollback /usr/bin/tilo-rollback
    inst_simple /usr/libexec/tilo-rollback-initrd-unlock
    inst_simple /usr/libexec/tilo-root-discover
    inst_simple /usr/lib/systemd/system-generators/tilo-root-generator
    inst_simple /usr/share/tilo/tpm2-pcr-signing.public.pem
}
