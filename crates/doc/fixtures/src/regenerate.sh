#!/bin/sh
# Regenerate the LibreOffice-authored office fixtures from the flat-ODF sources next to
# this script. Needs the LibreOffice flatpak (org.libreoffice.LibreOffice).
#
#   crates/doc/fixtures/src/regenerate.sh            # convert every src/*.fodt
#   crates/doc/fixtures/src/regenerate.sh --check F  # re-open F (an exported .docx/.odt) with LibreOffice
#
# Scratch output goes to ~/.cache/tilo-doc-fidelity (never /tmp).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repository=$(cd "$here/../../../.." && pwd)
out="${TILO_DOC_FIXTURE_OUT:-$repository/target/document-fixture-candidates}"
mkdir -p "$out"
scratch="${XDG_CACHE_HOME:-$HOME/.cache}/tilo-doc-fidelity"
mkdir -p "$scratch"

soffice() {
    flatpak run --command=libreoffice org.libreoffice.LibreOffice --headless "$@"
}

if [ "${1:-}" = "--check" ]; then
    shift
    for f in "$@"; do
        # One directory per format, so a .docx and an .odt of the same name don't collide.
        dir="$scratch/check/${f##*.}"
        soffice --convert-to txt:Text --outdir "$dir" "$f" >/dev/null
        txt="$dir/$(basename "${f%.*}").txt"
        if [ -s "$txt" ]; then
            echo "ok   $f ($(wc -c <"$txt") bytes of text)"
        else
            echo "FAIL $f" >&2
            exit 1
        fi
    done
    exit 0
fi

for src in "$here"/*.fodt; do
    name=$(basename "${src%.fodt}")
    for fmt in docx odt; do
        soffice --convert-to "$fmt" --outdir "$scratch" "$src" >/dev/null
        cp "$scratch/$name.$fmt" "$out/lo-$name.$fmt"
        echo "wrote $out/lo-$name.$fmt"
    done
done
