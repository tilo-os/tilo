#!/usr/bin/env python3
"""Build the opt-in, exactly pinned GTK compatibility component; never install it.

Run in the approved remote compatibility build environment. This does not fetch
headers, add a package, select an input method, or change a session environment.
"""

import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess


GTK_VERSION = (4, 22, 5)
SOURCE = Path(__file__).parent / "gtk_dictation" / "atomic_im.c"


def header_flags(root):
    if root is None:
        version = subprocess.check_output(
            ["pkg-config", "--modversion", "gtk4"], text=True, timeout=10
        ).strip()
        if version != ".".join(map(str, GTK_VERSION)):
            raise RuntimeError("GTK headers must match the qualified 4.22.5 runtime")
        return shlex.split(
            subprocess.check_output(
                ["pkg-config", "--cflags", "--libs", "gtk4", "gmodule-2.0"],
                text=True,
                timeout=10,
            )
        )
    text = (root / "usr/include/gtk-4.0/gtk/gtkversion.h").read_text()
    version = tuple(
        int(re.search(r"#define\s+GTK_" + part + r"_VERSION\s+\(?(\d+)", text)[1])
        for part in ["MAJOR", "MINOR", "MICRO"]
    )
    if version != GTK_VERSION:
        raise RuntimeError(
            "extracted headers must match the qualified GTK 4.22.5 runtime"
        )
    include = [
        "usr/include/gtk-4.0",
        "usr/include/pango-1.0",
        "usr/include/gdk-pixbuf-2.0",
        "usr/include/graphene-1.0",
        "usr/lib64/graphene-1.0/include",
    ]
    for directory in include:
        if not (root / directory).is_dir():
            raise RuntimeError("missing verified header directory: " + directory)
    flags = shlex.split(
        subprocess.check_output(
            ["pkg-config", "--cflags", "--libs", "gio-2.0", "gmodule-2.0", "cairo"],
            text=True,
            timeout=10,
        )
    )
    return (
        ["-I" + str(root / directory) for directory in include]
        + flags
        + ["-l:libgtk-4.so.1"]
    )


def build(args):
    gtk = ctypes.CDLL("libgtk-4.so.1")
    version = tuple(
        getattr(gtk, "gtk_get_" + part + "_version")()
        for part in ["major", "minor", "micro"]
    )
    if version != GTK_VERSION:
        raise RuntimeError("build runtime must match qualified GTK 4.22.5")
    flags = header_flags(args.headers_root)
    args.out.mkdir(parents=True, exist_ok=False)
    module = args.out / "immodules"
    module.mkdir()
    binary = module / "libtilo-atomic.so"
    # Keep the qualified source basename stable for ELF file symbols, and
    # retain the exact compilation input alongside its receipt.
    staged_source = args.out / "tilo-atomic.c"
    staged_source.write_bytes(SOURCE.read_bytes())
    # All flags are separate argv entries. Neither shell expansion nor host
    # installation is involved. Ordinary GTK input remains delegated.
    subprocess.run(
        [
            "cc",
            "-O2",
            "-fPIC",
            "-shared",
            "-Wall",
            "-Wextra",
            "-Werror",
            str(staged_source),
            "-o",
            str(binary),
            *flags,
        ],
        check=True,
        timeout=90,
    )
    manifest = {
        "component": "experimental-gtk4-native-commit-transaction",
        "gtk_version": list(GTK_VERSION),
        "source_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "binary_bytes": binary.stat().st_size,
        "input_method_name": "tilo-atomic",
        "gio_extension_point": "gtk-im-module",
        "default_priority": 0,
        "installed": False,
        "systemwide_activation": False,
        "header_custody": "caller must retain approved header/package provenance",
    }
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out", type=Path, required=True, help="fresh owned build output"
    )
    parser.add_argument(
        "--headers-root",
        type=Path,
        help="previously verified, privately extracted development RPM root",
    )
    build(parser.parse_args())


if __name__ == "__main__":
    main()
