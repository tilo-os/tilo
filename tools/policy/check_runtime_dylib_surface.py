#!/usr/bin/env python3
"""Keep OS internals and dead weight out of the shared app runtime.

Every thin app maps one Rust dylib, ``libruntime_dylib.so``
(crates/runtime/runtime-dylib), so each crate in it costs every app process
its load-time relocations and the memory they dirty, and every device
downloads it. Two rules hold for its dependency graph:

- **No OS internals.** The FUSE filesystem daemon, the sandbox launcher, the
  app host and the install CLI, the peer transport and the other daemons
  below are the OS's, never an app's. An app reaches them through a service;
  one that still calls into such a crate links it itself and gets only the
  code it calls.
- **No dead weight.** Every crate in the runtime is reached by at least one
  app of the default collection without going through the runtime.
- **No moved engine.** An engine that has a library of its own beside the
  runtime (crates/runtime/engines/) is loaded by its apps, or opened by the
  toolkit on first use; its crates never re-enter the runtime, through any
  dependency or app's feature. The workspace-hack's own edges do not count:
  its library names no crate, so they put nothing on the runtime's link
  line. Nor do the edges a crate names only without a feature the engines'
  build turns on (`LAZY_EDGES`).
- **No relocated table back.** A crate replaced because its tables cost
  every app process relocations (`TABLES`) stays out.

- **No hidden C library an app calls.** A crate that compiles a C library
  statically (``libsqlite3-sys`` with ``bundled``) puts that library inside
  the runtime, and a Rust dylib exports only Rust symbols. rustc then leaves
  the crate and its ``-l static=`` library off every app's link, so an app
  whose own code calls the C functions (rusqlite's ``#[inline]`` statement
  API, instantiated in imager-backend and db-client) cannot link. Every
  C-library crate in the runtime (``*-sys`` or one declaring Cargo's
  ``links``) is classified below; a new one fails until it is, and a library
  apps call themselves stays out of the runtime.

The graph is resolved the way the release builds it
(os/scripts/build-thin-apps.sh through dist/apps/package-apps.sh): the
runtime, every default-collection app and every engine in one Cargo graph,
each app and engine with its ``thin`` feature plus the collection's
features, for both shipped architectures. Feature unification is why this cannot be read from
``runtime-dylib``'s manifest alone: an app enabling a feature on a shared
crate can pull a daemon into the runtime through that crate.
"""

from __future__ import annotations

import collections
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = "runtime-dylib"
TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu")
# Crates the toolkit itself needs: if the resolved runtime lacks them, the
# check is not looking at the runtime and must fail rather than pass.
EXPECTED = frozenset({"tilo-ui", "tilo-app"})
# OS daemons and OS-internal crates. Each name is a Cargo package.
DENIED = {
    "tilo-fs": "the FUSE filesystem daemon",
    "fuser": "the FUSE filesystem daemon's kernel binding",
    "launcher": "the sandbox launcher",
    "app-host": "the app host daemon (apps use its client side)",
    "app-cli": "the install and registry CLI",
    "app-registry": "the registry protocol behind the app host",
    "install": "the package installer (tilo-install)",
    "iroh": "the peer transport, owned by the runtime daemon",
    "iroh-blobs": "the content store's transfer protocol",
    "iroh-gossip": "the filesystem's metadata gossip",
    "tilo-runtime": "the per-user runtime daemon",
    "tilo-relay": "the relay service",
    "shell": "the compositor",
    "ush": "the user shell daemon",
    "compat-service": "the Windows compatibility service",
    "local-model-service": "the on-device model service the model gateway runs (apps use "
                           "tilo-model-gateway without its default service feature)",
    "rten": "the subject model's runtime, which runs in the model gateway daemon",
}
# Crates of an engine that is a library of its own beside the runtime
# (crates/runtime/engines/): only its app maps them.
ENGINES = {
    "tilo-typst-engine": "Typst's engine library (libtilo_typst_engine.so)",
    "typst": "Typst, which Typst Editor loads from libtilo_typst_engine.so",
    "typst-library": "Typst's standard library, in libtilo_typst_engine.so",
    "typst-assets": "Typst's 9.6 MB font table, in libtilo_typst_engine.so",
    "tilo-spotify-engine": "Spotify playback's engine library (libtilo_spotify_engine.so)",
    "tilo-spotify": "Spotify playback, which Music loads from libtilo_spotify_engine.so",
    "librespot": "the Spotify client, in libtilo_spotify_engine.so",
    "tilo-paint-engine": "Canvas's painting engines' library (libtilo_paint_engine.so)",
    "tilo-wet-media": "the wet-media simulation, which Canvas loads from libtilo_paint_engine.so",
    "tilo-subject": "Canvas's subject mattes, in libtilo_paint_engine.so",
    "tilo-segment": "matte refinement under tilo-subject, in libtilo_paint_engine.so",
    "tilo-youtube-engine": "the YouTube client's engine library (libtilo_youtube_engine.so)",
    "rustypipe": "the YouTube client, which Music and Tube load from libtilo_youtube_engine.so",
    "rquickjs": "rustypipe's JavaScript engine, in libtilo_youtube_engine.so",
    "rquickjs-core": "rustypipe's JavaScript engine, in libtilo_youtube_engine.so",
    "rquickjs-sys": "QuickJS, rustypipe's JavaScript engine, in libtilo_youtube_engine.so",
    "tilo-accounts-engine": "the online-account engine library (libtilo_accounts_engine.so)",
    "account-custody": "online-account custody, which Mail, Settings and Calendar load from "
                       "libtilo_accounts_engine.so",
    "tilo-oauth": "the OAuth client under account-custody, in libtilo_accounts_engine.so",
    "cloud-source-dropbox": "a cloud-storage provider custody signs in to, in libtilo_accounts_engine.so",
    "cloud-source-gdrive": "a cloud-storage provider custody signs in to, in libtilo_accounts_engine.so",
    "cloud-source-icloud": "a cloud-storage provider custody signs in to, in libtilo_accounts_engine.so",
    "cloud-source-onedrive": "a cloud-storage provider custody signs in to, in libtilo_accounts_engine.so",
    "async-imap": "the IMAP client Mail and Settings load from libtilo_accounts_engine.so",
    "imap-proto": "the IMAP parser under async-imap, in libtilo_accounts_engine.so",
    "tilo-dav": "the WebDAV/CalDAV client, whose calendar sign-in reaches account custody, which "
                "Calendar and Settings load from libtilo_accounts_engine.so",
    "tilo-timezone-engine": "the time-zone database's engine library (libtilo_timezone_engine.so)",
    "chrono-tz": "the time-zone database, which Calendar and Clocks load from "
                 "libtilo_timezone_engine.so",
    "tilo-contact-directory-engine": "the contact directory's engine library "
                                     "(libtilo_contact_directory_engine.so)",
    "tilo-contact-directory": "the contact directory, which Contacts, Mail and Messages load from "
                              "libtilo_contact_directory_engine.so",
    "tilo-pdf-engine": "the PDF engine's library (libtilo_pdf_engine.so)",
    "tilo-pdf": "the PDF engine, which PDF Viewer, Files, Writer and Print Centre load from "
                "libtilo_pdf_engine.so",
    "hayro": "the in-process PDF rasteriser, in libtilo_pdf_engine.so",
    "hayro-interpret": "the in-process PDF rasteriser, in libtilo_pdf_engine.so",
    "hayro-jpeg2000": "the PDF rasteriser's JPEG 2000 decoder, in libtilo_pdf_engine.so",
    "pdfium-render": "the pdfium helper's bindings, in libtilo_pdf_engine.so",
    "tilo-highlight-engine": "the syntax grammars' engine library (libtilo_highlight_engine.so)",
    "tilo-highlight-grammars": "the syntax grammars, which the toolkit opens from "
                               "libtilo_highlight_engine.so the first time code is highlighted",
    "syntect": "the syntax highlighter, in libtilo_highlight_engine.so",
    "two-face": "the packed syntax grammars, in libtilo_highlight_engine.so",
    "tilo-adblock-engine": "the ad-block compiler's engine library (libtilo_adblock_engine.so)",
    "tilo-content-blocker": "the ad-block compiler, which the toolkit opens from "
                            "libtilo_adblock_engine.so the first time an app compiles lists",
    "adblock": "adblock-rust, in libtilo_adblock_engine.so",
    "tilo-fuse-engine": "the FUSE presentation's engine library (libtilo_fuse_engine.so)",
    "runtime-fs-fuse": "the workspace filesystem's FUSE presentation, which Imager's projection "
                       "worker loads from libtilo_fuse_engine.so",
}

# C-library crates (a `-sys` name or Cargo's `links` key) the runtime may
# carry, by what linking them does to an app. Each name is a Cargo package.
# - system: binds a shared system library; rustc passes its `-l` to every app
#   that names the crate, so an app's own calls resolve against that library.
# - bindings: no C library is linked (syscall or platform-framework bindings,
#   a `links` key kept only for uniqueness, or a build-time crate).
# - bundled: compiles a static C library into the runtime, whose symbols the
#   runtime does not export. Allowed only while apps reach the C functions
#   through non-inline, non-generic Rust code compiled into the runtime; the
#   reason says why that holds. build-thin-apps.sh's links fail otherwise.
C_LIBRARIES = {
    "gio-sys": ("system", "GIO, loaded on first use (runtime-dylib-lazy-link.py)"),
    "glib-sys": ("system", "GLib, loaded on first use"),
    "gobject-sys": ("system", "GObject, loaded on first use"),
    "gstreamer-sys": ("system", "GStreamer, loaded on first use"),
    "gstreamer-app-sys": ("system", "GStreamer, loaded on first use"),
    "gstreamer-base-sys": ("system", "GStreamer, loaded on first use"),
    "gstreamer-video-sys": ("system", "GStreamer, loaded on first use"),
    "pipewire-sys": ("system", "libpipewire"),
    "wayland-sys": ("system", "libwayland"),
    "libudev-sys": ("system", "libudev"),
    "libz-sys": ("system", "the system zlib (no static feature)"),
    "renderdoc-sys": ("bindings", "RenderDoc, opened with dlopen when present"),
    "linux-raw-sys": ("bindings", "Linux kernel ABI definitions"),
    "dirs-sys": ("bindings", "libc and platform calls"),
    "inotify-sys": ("bindings", "libc inotify calls"),
    "netlink-sys": ("bindings", "netlink sockets"),
    "fsevent-sys": ("bindings", "macOS framework"),
    "kqueue-sys": ("bindings", "BSD/macOS kqueue"),
    "core-foundation-sys": ("bindings", "macOS framework"),
    "security-framework-sys": ("bindings", "macOS framework"),
    "system-configuration-sys": ("bindings", "macOS framework"),
    "objc-sys": ("bindings", "the Objective-C runtime on Apple targets"),
    "jni-sys": ("bindings", "Android JNI"),
    "ndk-sys": ("bindings", "Android NDK"),
    "windows-sys": ("bindings", "Windows API"),
    "js-sys": ("bindings", "wasm32 JavaScript bindings"),
    "web-sys": ("bindings", "wasm32 web bindings"),
    "wasm-bindgen-shared": ("bindings", "`links` for uniqueness only"),
    "rayon-core": ("bindings", "`links` for uniqueness only"),
    "prettyplease": ("bindings", "`links` for uniqueness only"),
    "defmt": ("bindings", "`links` for uniqueness only"),
    "clang-sys": ("bindings", "bindgen's build-time libclang"),
    "zstd-sys": ("bundled", "zstd's C, called only from zstd-safe's non-generic functions"),
    "ring": ("bundled", "ring's C and assembly, behind ring's non-generic API"),
    "blake3": ("bundled", "BLAKE3's SIMD C and assembly, behind blake3's non-generic API"),
    "libspa-sys": ("bundled", "wrappers of SPA's static inline functions, called by libspa"),
}
# C libraries whose Rust API runs C calls in the calling crate, so the apps
# that use them link them themselves and the runtime never carries them.
APP_LINKED = {
    "libsqlite3-sys": "SQLite's bundled C library",
    "rusqlite": "SQLite's Rust API, whose #[inline] statement methods call sqlite3_* "
                "from the app's own code",
}


def links_keys() -> frozenset[str]:
    """Packages declaring Cargo's `links` key: each links a native library."""
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo metadata failed:\n{result.stderr}")
    return frozenset(
        package["name"] for package in json.loads(result.stdout)["packages"] if package.get("links")
    )


# Edges a crate's code names only while a feature is off. The feature is one
# the engines' `thin` turns on in the release graph (the toolkit opens the
# engine on first use instead), and rustc links no crate the code does not
# name, so with it on the edge puts nothing on the runtime's link line.
LAZY_EDGES = {
    ("tilo-ui", "tilo-highlight-grammars"): "lazy-highlight",
    ("tilo-ui", "tilo-content-blocker"): "lazy-adblock",
}
# Crates replaced because their tables are pointers the loader relocates in
# every app process before main.
TABLES = {
    "mime_guess": "an extension table of ~1400 string slices; the runtime looks types up "
                  "through tilo-mime",
    "bip39": "a wordlist of 2048 string slices; threshold-identity encodes recovery phrases "
             "itself",
}


def engines() -> list[str]:
    """Every runtime engine's package, as build-thin-apps.sh finds them."""
    names = []
    for manifest in sorted((ROOT / "crates/runtime/engines").glob("*/Cargo.toml")):
        match = re.search(r'^name = "([^"]+)"', manifest.read_text(), re.M)
        if match:
            names.append(match.group(1))
    return names


def collection() -> tuple[list[str], list[str]]:
    """The default collection's packages and their release features."""
    result = subprocess.run(
        [sys.executable, "tools/apps/app_release.py", "--collection", "default"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"tools/apps/app_release.py failed:\n{result.stderr}")
    plan = json.loads(result.stdout)
    packages = list(plan["packages"])
    if not packages:
        raise RuntimeError("the default collection has no packages")
    return packages, [f"{package}/thin" for package in packages] + list(plan["features"])


def resolve(packages: list[str], features: list[str], target: str) -> str:
    command = ["cargo", "tree", "--locked", "--edges", "normal", "--target", target,
               "--prefix", "depth", "--format", "{p}|{f}", "--package", RUNTIME]
    engine_packages = engines()
    for package in packages + engine_packages:
        command += ["--package", package]
    command += ["--features", ",".join(features + [f"{engine}/thin" for engine in engine_packages])]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"cargo tree failed for {target}:\n{result.stderr}")
    return result.stdout


def parse(tree: str) -> tuple[list[str], dict[str, set[str]]]:
    """Roots and edges of a ``--prefix depth`` forest, keyed by package name.

    A deduplicated tree prints each package's dependencies once, under its
    first appearance, so the edges are complete even where a later
    appearance is marked ``(*)``. A line may carry the package's features
    after a ``|`` (``--format "{p}|{f}"``); an edge of ``LAZY_EDGES`` whose
    feature the package has on is left out.
    """
    roots: list[str] = []
    edges: dict[str, set[str]] = collections.defaultdict(set)
    features: dict[str, set[str]] = {}
    stack: list[str] = []
    for line in tree.splitlines():
        match = re.match(r"^(\d+)(\S+)", line)
        if not match:
            continue
        depth, name = int(match.group(1)), match.group(2)
        if "|" in line and name not in features:
            listed = line.split("|", 1)[1].replace("(*)", "").strip()
            features[name] = {feature for feature in listed.split(",") if feature}
        del stack[depth:]
        if depth == 0:
            roots.append(name)
        elif stack:
            edges[stack[-1]].add(name)
        stack.append(name)
    for (parent, child), feature in LAZY_EDGES.items():
        if feature in features.get(parent, set()):
            edges.get(parent, set()).discard(child)
    return roots, edges


# The workspace-hack's library names none of its dependencies, and rustc
# links only the crates a crate names (an `--extern` nothing names is never
# loaded), so the hack's edges add no crate to a link: crates only it reaches
# are on no link line of the runtime. What the hack does change is features,
# and a feature that adds a dependency shows up as that crate's own edge.
HACK = "workspace-hack"


def reached(edges: dict[str, set[str]], start: str, through_runtime: bool = True) -> set[str]:
    seen: set[str] = set()
    pending = [start]
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        for dependency in edges.get(name, ()):
            if dependency == HACK:
                continue
            if through_runtime or dependency != RUNTIME:
                pending.append(dependency)
    return seen


def violations(tree: str, apps: list[str], target: str,
               links: frozenset[str] = frozenset()) -> list[str]:
    roots, edges = parse(tree)
    if RUNTIME not in roots:
        return [f"cargo tree did not describe {RUNTIME} for {target}"]
    runtime = reached(edges, RUNTIME) - {RUNTIME}
    missing = sorted(EXPECTED - runtime)
    if missing:
        return [f"{RUNTIME} for {target} lacks {', '.join(missing)}; this is not the runtime graph"]
    found = []
    for name in sorted(runtime & DENIED.keys()):
        found.append(
            f"{RUNTIME} reaches {name}, {DENIED[name]}, for {target}: OS internals are not app "
            "runtime; an app reaches them through a service or links what it calls itself"
        )
    for name in sorted(runtime & TABLES.keys()):
        found.append(
            f"{RUNTIME} reaches {name}, {TABLES[name]}, for {target}: its tables would cost every "
            "app process relocations before main (docs/measurements/runtime-size.md)"
        )
    for name in sorted(runtime & ENGINES.keys()):
        found.append(
            f"{RUNTIME} reaches {name}, {ENGINES[name]}, for {target}: an engine with a library "
            "of its own stays out of the runtime every app maps; keep it out of the runtime's "
            "dependencies and of the features apps enable on them (docs/measurements/runtime-size.md)"
        )
    for name in sorted(runtime & APP_LINKED.keys()):
        found.append(
            f"{RUNTIME} reaches {name}, {APP_LINKED[name]}, for {target}: the runtime does not "
            "export a bundled C library's symbols, so an app calling into it cannot link; apps "
            "that use it link it themselves (keep it out of the runtime's dependencies and of "
            "the features apps enable on them)"
        )
    c_libraries = {name for name in runtime if name.endswith("-sys") or name in links}
    for name in sorted(c_libraries - C_LIBRARIES.keys() - APP_LINKED.keys()):
        found.append(
            f"{RUNTIME} reaches {name}, a C-library crate, for {target}: classify it in "
            "C_LIBRARIES (tools/policy/check_runtime_dylib_surface.py). A static C library in "
            "the runtime is not exported, so an app whose own code calls it fails to link"
        )
    absent = sorted(set(apps) - set(roots))
    if absent:
        return found + [f"cargo tree did not describe {', '.join(absent)} for {target}"]
    used: set[str] = set()
    for app in apps:
        used |= reached(edges, app, through_runtime=False)
    for name in sorted(runtime - used):
        found.append(
            f"{RUNTIME} carries {name} for {target}, but no default-collection app reaches it: "
            "every app process would map it for nothing"
        )
    return found


def main() -> int:
    try:
        apps, features = collection()
        links = links_keys()
        failed = False
        for target in TARGETS:
            found = violations(resolve(apps, features, target), apps, target, links)
            for message in found:
                print(message, file=sys.stderr)
            failed = failed or bool(found)
            if not found:
                print(f"{RUNTIME} ({target}): no OS internals, moved engines or app-linked C libraries, "
                      "every crate used by an app")
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
