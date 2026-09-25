#!/usr/bin/sh

# Tilo's initrd generator resolves this target-independent token after dracut's
# command-line phase. Tell dracut that the exact token has a registered owner.
# shellcheck disable=SC2154 # dracut defines root before sourcing cmdline hooks
case "$root" in
    tilo-auto)
        # shellcheck disable=SC2034
        rootok=1
        ;;
esac
