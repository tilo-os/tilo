#!/usr/bin/bash

check() {
    require_binaries qos-rollback cryptsetup systemd-ask-password systemd-escape || return 1
    return 255
}

depends() {
    echo "systemd-cryptsetup systemd-ask-password tpm2-tss"
    return 0
}

install() {
    inst_multiple qos-rollback cryptsetup systemd-ask-password systemd-escape \
        grep tpm2_createprimary tpm2_flushcontext tpm2_getpolicydigest \
        tpm2_load tpm2_loadexternal tpm2_nvread tpm2_nvreadpublic \
        tpm2_policyauthorize tpm2_policyauthvalue tpm2_policynv \
        tpm2_policypcr tpm2_policyor tpm2_readpublic tpm2_startauthsession \
        tpm2_unseal tpm2_verifysignature
    inst_simple /usr/bin/qos-rollback /usr/bin/qos-rollback
    inst_simple /usr/libexec/qos-rollback-initrd-unlock
    inst_simple /usr/lib/systemd/system-generators/qos-rollback-unlock-generator
    inst_simple /usr/share/qos/tpm2-pcr-signing.public.pem
}
