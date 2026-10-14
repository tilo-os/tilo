#!/bin/sh
# Kbuild's system_certificates.S, x86 piggy.S and x86 sbat.S use .incbin for
# generated inputs. Compiler caches do not necessarily track those assembler
# inputs. Stale objects can embed an old signer or SBAT data in the boot image.
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
    esac
done
exec "${TILO_KERNEL_CC_LAUNCHER:?expected compiler cache}" "$compiler" "$@"
