#!/usr/bin/bash
# Assert that the binaries about to be shipped actually carry the link-time
# hardening the build claims to set. Run over the compiled output, not the
# source: the point is that a flag was silently dropped somewhere between
# `.cargo/config.toml` and the linker, and only the ELF can say.
#
#   verify-elf-hardening.sh /out           # every ELF in a directory
#   verify-elf-hardening.sh a.out b.out    # named files
#
# Exits non-zero on the first binary that fails a required property, so this
# can gate an image build.
set -uo pipefail

fail=0
checked=0

# --- required properties ----------------------------------------------------
#
# pie          Position-independent executable, so the kernel can place the
#              image (and therefore the GOT, PLT and code) at a random base.
#              Without it ASLR does nothing for the main binary.
# relro        A PT_GNU_RELRO segment: the relocated data the loader is done
#              with gets mapped read-only, which takes the GOT off the list of
#              things a heap overflow can rewrite into a code pointer.
# bindnow      Every symbol bound at load, so there is no writable PLT to
#              hijack mid-run. RELRO without this is "partial RELRO" and leaves
#              precisely the interesting entries writable.
# noexecstack  PT_GNU_STACK without the X bit. One object file requesting an
#              executable stack flips it for the whole process, which is why
#              this is checked on the linked output.
# shstk        GNU_PROPERTY_X86_FEATURE_1_SHSTK. glibc turns hardware shadow
#              stacks on only when the executable and every DSO it loads carry
#              the mark, so one unmarked binary opts the whole process out
#              silently — there is no error, the protection just isn't there.
#              Stable Rust emits no property note at all, hence the explicit
#              `-Wl,-z,shstk` in .cargo/config.toml.
# noopenssl    No direct DT_NEEDED edge to libssl or libcrypto. Tilo-owned TLS
#              and signing paths are Rust-native; checking the final ELF catches
#              feature unification or a native dependency that source grep and
#              Cargo manifest review cannot see.
#
# Deliberately NOT required: IBT. endbr64 landing pads have to come from the
# compiler and stable rustc does not emit them, so a binary claiming IBT would
# trap on its first indirect call.

# --- informational ----------------------------------------------------------
#
# stackprot    A reference to __stack_chk_fail proves -fstack-protector-strong
#              reached *some* compiled-in C. Rust code does not use canaries
#              (it has bounds checks instead), so a pure-Rust binary legitimately
#              has none and this can only ever be reported, not required.

is_elf() {
    # `file` is not installed in the build stage; read the magic directly.
    local magic=
    LC_ALL=C IFS= read -r -N 4 magic <"$1" 2>/dev/null
    [ "$magic" = $'\x7fELF' ]
}

# Each binary is read by one readelf, matched in the shell: a run over the image's
# 170 binaries spent minutes starting a grep per property.
check_one() {
    local bin="$1" name="${1##*/}" errs=() elf nl=$'\n'
    # Headers, program headers, dynamic section and notes.
    elf="$(readelf -hlndW "$bin" 2>/dev/null)"

    [[ $elf =~ Type:[^$nl]*DYN ]] || errs+=("not a PIE (ET_DYN)")
    [[ $elf == *GNU_RELRO* ]] || errs+=("no PT_GNU_RELRO segment")
    [[ $elf == *BIND_NOW* || $elf =~ Flags:[^$nl]*[[:space:]]NOW([[:space:]]|$) ]] \
        || errs+=("not bound at load (no BIND_NOW / FLAGS_1 NOW)")
    if [[ $elf =~ Shared\ library:\ \[lib(ssl|crypto)\.so[.0-9]*\] ]]; then
        errs+=("direct OpenSSL ABI dependency (DT_NEEDED libssl/libcrypto)")
    fi

    # PT_GNU_STACK line: columns end with the flags field, e.g. "RW  0x10".
    if [[ $elf =~ GNU_STACK[^$nl]* ]] && [[ ${BASH_REMATCH[0]} =~ [[:space:]](RWE|E)[[:space:]] ]]; then
        errs+=("executable stack requested (PT_GNU_STACK carries X)")
    fi

    local cet="not applicable"
    if [[ $elf =~ Machine:[^$nl]*X86-64 ]]; then
        cet="SHSTK-marked"
        [[ $elf == *SHSTK* ]] \
            || errs+=("no shadow-stack property (GNU_PROPERTY_X86_FEATURE_1_SHSTK)")
    fi

    local canary="no C canaries"
    # Counted, not `grep -q`: an early exit would fail readelf's write under pipefail.
    [[ $(readelf -W --dyn-syms "$bin" 2>/dev/null | grep -cF '__stack_chk_fail') != 0 ]] \
        && canary="stack protector in linked C"

    checked=$((checked + 1))
    if [ ${#errs[@]} -eq 0 ]; then
        printf '[ OK ]   %-24s PIE, full RELRO, noexecstack, no OpenSSL, %s (%s)\n' "$name" "$cet" "$canary"
    else
        fail=1
        printf '[FAIL]   %-24s %s\n' "$name" "${errs[0]}"
        local i
        for ((i = 1; i < ${#errs[@]}; i++)); do
            printf '         %-24s %s\n' "" "${errs[i]}"
        done
    fi
}

targets=()
for arg in "$@"; do
    if [ -d "$arg" ]; then
        while IFS= read -r f; do targets+=("$f"); done < <(find "$arg" -maxdepth 1 -type f -executable | sort)
    else
        targets+=("$arg")
    fi
done

if [ ${#targets[@]} -eq 0 ]; then
    echo "usage: $(basename "$0") <dir|binary> [binary...]" >&2
    exit 2
fi

echo "=== ELF hardening ==="
for t in "${targets[@]}"; do
    is_elf "$t" || continue
    check_one "$t"
done

if [ "$checked" -eq 0 ]; then
    echo "no ELF binaries found in: $*" >&2
    exit 2
fi

if [ "$fail" -ne 0 ]; then
    echo "=== $checked binaries checked — HARDENING MISSING (see [FAIL] above) ==="
    exit 1
fi
echo "=== $checked binaries checked, all hardened ==="
