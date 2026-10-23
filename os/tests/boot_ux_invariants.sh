#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
uki="$root/os/dev/uki.sh"
kargs="$root/os/usr/lib/bootc/kargs.d/03-boot.toml"
containerfile="$root/os/Containerfile"
theme="$root/os/usr/share/plymouth/themes/tilo/tilo.plymouth"
script="$root/os/usr/share/plymouth/themes/tilo/tilo.script"

grep -Fq -- '--splash /out/tilo-splash.bmp' "$uki"
grep -Fq 'make-boot-splash.py /out/tilo-splash.bmp' "$uki"
# Check the installed path, including grouped COPY instructions.
# shellcheck source=lib/containerfile.sh
. "$root/os/tests/lib/containerfile.sh"
if ! containerfile_has_copy "$containerfile" os/dev/make-boot-splash.py /usr/local/bin/make-boot-splash.py; then
  printf 'image must install the boot splash generator at its runtime path\n' >&2
  exit 1
fi
grep -Fq 'plymouth-plugin-script' "$containerfile"
if ! grep -Fq -- '--add "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock tilo-boot-failure"' "$root/os/scripts/build-initramfs.sh" \
  || ! grep -Fq 'bash /run/tilo-build-initramfs.sh "$kver"' "$containerfile"; then
  printf 'initramfs must explicitly include crypt, TPM, Plymouth, and Tilo rollback unlock modules\n' >&2
  exit 1
fi
grep -Fq 'plymouth-set-default-theme tilo' "$containerfile"
grep -Fq 'ModuleName=script' "$theme"
grep -Fq 'Plymouth.SetDisplayPasswordFunction(display_password)' "$script"
grep -Fq 'if (!global.dialog)' "$script"
grep -Fq 'global.dialog.heading = Sprite' "$script"
grep -Fq 'global.dialog.entry = Sprite' "$script"
grep -Fq 'global.dialog.ready = 1' "$script"
# The splash starts on the firmware framebuffer and moves to the GPU driver's
# native mode mid-boot. Positions computed for the first screen would leave
# the splash off-centre on the second, so a display change lays it out again.
grep -Fq 'Plymouth.SetDisplayHotplugFunction(display_hotplug)' "$script"
python3 - "$script" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
def body(name):
    match = re.search(r"^fun " + name + r"\(.*?^  }$", text, re.S | re.M)
    assert match, f"tilo.script has no {name}()"
    return match.group(0)
assert "place_dialog();" in body("display_password"), \
    "the unlock prompt must be positioned for the current screen each time it is shown"
assert "SetPosition" not in body("setup_dialog"), \
    "setup_dialog() creates sprites; place_dialog() positions them for the current screen"
hotplug = body("display_hotplug")
assert "display_password" in hotplug and "display_normal" in hotplug, \
    "a display change must re-run the layout of whichever screen is showing"
PY
if grep -Eq '^[[:space:]]+dialog\.(heading|help|entry|lock|bullet_image)[[:space:]]*=' "$script"; then
  printf 'Plymouth dialog state must be assigned globally from callbacks\n' >&2
  exit 1
fi
if grep -Fq 'plymouth-quit.service' "$containerfile"; then
  printf 'Plymouth quit services must not be masked\n' >&2
  exit 1
fi

for arg in quiet splash plymouth.ignore-serial-consoles loglevel=3 systemd.show_status=auto rd.udev.log_level=3 vt.global_cursor_default=0; do
  grep -Fq "\"$arg\"" "$kargs" || {
    printf 'missing boot UX argument: %s\n' "$arg" >&2
    exit 1
  }
done

scratch="${CARGO_TARGET_DIR:-$root/target}"
mkdir -p "$scratch"
tmp="$(mktemp -d "$scratch/boot-ux.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
python3 "$root/os/dev/make-boot-splash.py" "$tmp/splash.bmp"
python3 "$root/os/dev/make-plymouth-assets.py" "$tmp/plymouth" "$root/os/usr/share/tilo/boot-failure/locales"
python3 - "$tmp/splash.bmp" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read()
assert data[:2] == b"BM"
width, height = struct.unpack_from("<ii", data, 18)
assert (width, height) == (1024, 768)
assert len(set(data[54:])) > 24, "splash is visually empty"
# systemd-stub clears the screen to black and centres this tile at the
# firmware's mode; a non-black edge shows as a box on any larger panel.
stride = (width * 3 + 3) & ~3
def pixel(x, y):
    at = 54 + (height - 1 - y) * stride + x * 3
    return tuple(data[at:at + 3])
edge = [pixel(x, y) for x in range(width) for y in (0, height - 1)]
edge += [pixel(x, y) for y in range(height) for x in (0, width - 1)]
assert all(value == (0, 0, 0) for value in edge), \
    "the UKI splash tile must be black at its edge, like the stub's fill around it"
PY
# Both boot screens draw the brand's mark from the same path data, so the
# stub's tile and the Plymouth theme cannot drift apart.
python3 - "$root/os/dev/make-boot-splash.py" "$root/os/dev/make-plymouth-assets.py" <<'PY'
import sys
def mark(path):
    text = open(path).read()
    start = text.index("MARK_SIDES = (")
    return text[start:text.index("\n\n\n", start)]
splash, theme = (mark(path) for path in sys.argv[1:])
assert splash == theme, "the UKI splash and the Plymouth theme draw different marks"
assert "M10.98 31.85 Q10.979892448752274 27.96648782251284 14.4 26.12" in splash
assert "MARK_BLOCK = (29.0, 16.0, 71.0, 84.0, 6.0)" in splash
PY
if grep -Fq 'recognisable q' "$root/os/dev/make-boot-splash.py" \
   || grep -Fq 'tail_distance' "$root/os/dev/make-plymouth-assets.py" \
   || grep -Fq 'enumerate((56.0, 110.0, 164.0))' "$root/os/dev/make-plymouth-assets.py"; then
  printf 'boot artwork still carries a retired Tilo mark\n' >&2
  exit 1
fi
python3 - "$tmp/plymouth" <<'PY'
import pathlib, struct, sys
root = pathlib.Path(sys.argv[1])
expected = {"tilo-logo.png", "entry.png", "lock.png", "bullet.png",
            "unlock-heading.png", "unlock-help.png", "unlock-retry.png",
            "starting-tilo.png", "shutting-down.png", "startup-help.png",
            "progress-background.png", "progress-fill.png"}
assert {path.name for path in root.iterdir()} == expected | {"failure"}
for path in root.rglob("*.png"):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    width, height = struct.unpack(">II", data[16:24])
    assert width > 0 and height > 0, path
PY

# The boot-failure screen: every word tilo-boot-failure can ask tilo.script to
# draw is rendered for every locale, and the glyph atlases are indexed the
# same way on both sides.
python3 - "$tmp/plymouth/failure" "$root/os/usr/share/tilo/boot-failure/locales" \
  "$root/os/dev/make-plymouth-assets.py" "$script" <<'PY'
import pathlib, re, sys
out, locales = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
generator, script = (pathlib.Path(p).read_text() for p in sys.argv[3:5])
glyphs = re.search(r'FAILURE_GLYPHS = \(("[^"]*")\s*("[^"]*")\)', generator)
glyphs = eval(glyphs.group(1)) + eval(glyphs.group(2))
assert f'failure.glyphs = "{glyphs}";' in script, "tilo.script and the generator index the glyphs differently"
assert not set('"\\|') & set(glyphs)
for atlas in ("a", "m"):
    assert len(list((out / atlas).glob("*.png"))) == len(glyphs), atlas
assert (out / "button.png").is_file() and (out / "button-focus.png").is_file()
catalogues = {}
for ftl in sorted(locales.glob("*.ftl")):
    keys = {}
    for line in ftl.read_text().splitlines():
        key, sep, value = line.partition(" = ")
        if sep and re.fullmatch(r"[a-z0-9-]+", key):
            keys[key] = value
    catalogues[ftl.stem] = keys
    for key, value in keys.items():
        if key.startswith("boot-failure-console-"):
            continue
        short = key.removeprefix("boot-failure-")
        pieces = re.split(r"\{ \$[a-z]+ \}", value)
        names = [f"{short}.png"] if len(pieces) == 1 else [f"{short}.{i}.png" for i in range(len(pieces))]
        for name in names:
            assert (out / ftl.stem / name).is_file(), (ftl.stem, name)
english = set(catalogues["en-US"])
for locale, keys in catalogues.items():
    assert set(keys) == english, f"{locale} lacks or adds boot-failure words: {set(keys) ^ english}"
PY

# Every image build boots its own UKI on a panel larger than the firmware's
# mode and films the splash through the disk unlock.
grep -Fq 'os/dev/check-boot-splash.py' "$root/.forgejo/workflows/build-image.yml"

printf 'boot UX invariants hold\n'
