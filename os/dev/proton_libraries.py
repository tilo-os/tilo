#!/usr/bin/python3
"""Check that every library the pinned Proton loads on x86 is there.

A capsule starts Proton's own script on the system's libraries, the way Wine
is started, never in the Steam runtime's container (that container starts a sandbox
of its own, which the capsule's sandbox refuses). So whatever Proton's Wine
links or loads by name must come from the base image, the Windows
compatibility extension or Proton itself. The extension's build calls this with
the base image as `/` and the extension's payload as TREE, and fails naming
each library nothing provides.

    proton_libraries.py PROTON --tree TREE

ARM translates Proton through the Steam runtime, which brings its own
libraries: there is nothing to check.
"""

import argparse
import platform
import subprocess
import sys
from pathlib import Path

LOADER = "/lib64/ld-linux-x86-64.so.2"

# What Wine inside Proton loads by name, once a program uses what it serves, and
# which therefore no ELF lists as needed: windows (X11 and its extensions),
# drawing (Vulkan, OpenGL and EGL), sound (ALSA, PulseAudio), text (FreeType,
# Fontconfig), TLS (GnuTLS), game controllers (SDL, udev, libusb), D-Bus and
# printing (CUPS). Loaders it tries without needing them (CUDA, ODBC, Samba's
# netapi, OSMesa, the speech models' extra ONNX providers) are left out.
LOADED_BY_NAME = (
    "libX11.so.6",
    "libX11-xcb.so.1",
    "libXext.so.6",
    "libXrender.so.1",
    "libXi.so.6",
    "libXrandr.so.2",
    "libXcursor.so.1",
    "libXcomposite.so.1",
    "libXfixes.so.3",
    "libXinerama.so.1",
    "libXxf86vm.so.1",
    "libxcb.so.1",
    "libvulkan.so.1",
    "libGL.so.1",
    "libGLX.so.0",
    "libEGL.so.1",
    "libasound.so.2",
    "libpulse.so.0",
    "libfreetype.so.6",
    "libfontconfig.so.1",
    "libgnutls.so.30",
    "libdbus-1.so.3",
    "libudev.so.1",
    "libSDL2-2.0.so.0",
    "libusb-1.0.so.0",
    "libcups.so.2",
)

# Proton's own script is started with the system's Python.
INTERPRETER = "usr/bin/python3"


def is_x86_64_elf(path):
    """An x86-64 ELF: class 64-bit (EI_CLASS 2) and machine EM_X86_64 (62)."""
    try:
        with path.open("rb") as stream:
            header = stream.read(20)
    except OSError:
        return False
    return (
        len(header) == 20
        and header[:4] == b"\x7fELF"
        and header[4] == 2
        and int.from_bytes(header[18:20], "little") == 62
    )


def proton_elves(proton):
    """Proton's 64-bit Unix side: its programs, Wine's Unix modules and its own libraries."""
    files = proton / "files"
    candidates = [
        *(files / "bin").glob("*"),
        *(files / "lib/wine/x86_64-unix").glob("*.so"),
        *(files / "lib/x86_64-linux-gnu").rglob("*.so*"),
    ]
    return sorted(
        path for path in candidates if path.is_file() and not path.is_symlink() and is_x86_64_elf(path)
    )


def loader_listing(command):
    """The loader's listing of what a file needs, found or not."""
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True) as loader:
        return loader.communicate()[0]


def unresolved(path, library_path, listing=loader_listing):
    """What `path` needs that the loader finds nowhere, from the loader's own listing."""
    listed = listing([LOADER, "--library-path", library_path, "--list", str(path)])
    return [line.split()[0] for line in listed.splitlines() if "not found" in line]


def library_directories(proton, tree, base):
    return [
        proton / "files/lib/x86_64-linux-gnu",
        proton / "files/lib/wine/x86_64-unix",
        tree / "usr/lib64",
        base / "usr/lib64",
        base / "lib64",
    ]


def missing(proton, tree, base=Path("/"), listing=loader_listing):
    """Every library Proton needs or loads by name that no directory provides, sorted."""
    directories = library_directories(proton, tree, base)
    library_path = ":".join(str(directory) for directory in directories)
    absent = set()
    for path in proton_elves(proton):
        absent.update(unresolved(path, library_path, listing))
    for name in LOADED_BY_NAME:
        if not any((directory / name).exists() for directory in directories):
            absent.add(name)
    if not any((root / INTERPRETER).exists() for root in (tree, base)):
        absent.add("/" + INTERPRETER)
    return sorted(absent)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("proton", type=Path)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--base", type=Path, default=Path("/"))
    parser.add_argument("--arch", default=platform.machine())
    args = parser.parse_args()
    if args.arch != "x86_64":
        print(f"proton libraries: {args.arch} translates Proton through the Steam runtime")
        return 0
    if not proton_elves(args.proton):
        print(f"proton libraries: no Proton at {args.proton}", file=sys.stderr)
        return 1
    absent = missing(args.proton, args.tree, args.base)
    if absent:
        print("proton libraries: nothing provides " + ", ".join(absent), file=sys.stderr)
        return 1
    print(f"proton libraries: all {len(LOADED_BY_NAME)} loaded by name and every needed one found")
    return 0


if __name__ == "__main__":
    sys.exit(main())
