#!/bin/sh
# A compiler cache keys an object by its preprocessed source and flags, so it
# does not see a file that source includes with the assembler's `.incbin`: the
# signer certificate in system_certificates.S, the compressed kernel in x86's
# piggy.S, the vDSO in arm64's vdso-wrap.S, the config and headers archives in
# kernel/configs.c and kernel/kheaders.c, built-in firmware. A cached object
# would embed an old copy (an old signer, a stale vDSO). Every source that names
# .incbin, and the generated ones listed, compiles without the cache.
set -eu
compiler=${1:?expected compiler}
shift
# A leading -E is unambiguously preprocessing-only. Do not interpret later
# operands or option values: those retain the launcher's existing parser.
case "${1-}" in
    -E) exec "$compiler" "$@" ;;
esac
for argument do
    case "$argument" in
        certs/system_certificates.S|*/certs/system_certificates.S|arch/x86/boot/compressed/piggy.S|*/arch/x86/boot/compressed/piggy.S|arch/x86/boot/compressed/sbat.S|*/arch/x86/boot/compressed/sbat.S)
            exec "$compiler" "$@"
            ;;
        *.c|*.S|*.s)
            if [ -f "$argument" ] && grep -q 'incbin' "$argument"; then
                exec "$compiler" "$@"
            fi
            ;;
    esac
done
exec "${TILO_KERNEL_CC_LAUNCHER:?expected compiler cache}" "$compiler" "$@"
