#!/bin/sh
# Kbuild's system_certificates.S uses .incbin for the per-build DER certificate.
# Compiler caches do not necessarily track that assembler input. A cached
# system_certificates.o from another build would make the kernel reject every
# module signed with this build's fresh key.
set -eu
compiler=${1:?expected compiler}
shift
for argument do
    case "$argument" in
        certs/system_certificates.S|*/certs/system_certificates.S)
            exec "$compiler" "$@"
            ;;
    esac
done
exec "${TILO_KERNEL_CC_LAUNCHER:?expected compiler cache}" "$compiler" "$@"
