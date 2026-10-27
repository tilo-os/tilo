#!/usr/bin/env python3
"""What a built OS image must hold before anyone boots it, checked inside the image.

    podman run --rm --network=none -v os/tests/image_smoke.py:/smoke.py:ro IMAGE python3 -I /smoke.py

tools/ci/publish/os-image.sh smoke runs it on the image os-image-ARCH built. The build checks
what each stage needs while it runs; later stages then remove packages and plugins on purpose,
and a package-set change can drop a library something else links. This looks at the finished
tree, as a device gets it:

  libraries   every ELF under the system paths resolves each library it links (ldd)
  scripts     every script's interpreter exists
  rpm         every installed package's dependencies are installed
  units       every enabled unit exists and `systemd-analyze verify` accepts it
  gstreamer   no plugin fails to load, the elements Tilo builds pipelines from exist, and
              Miracast's media path runs: AAC (and H.264 where an encoder is installed) muxed
              to MPEG-TS and sent as RTP
  programs    the external programs the desktop runs start

Exit 0 when all pass, 1 when any fails; each failure names what and why.
"""

from __future__ import annotations

import concurrent.futures
import os
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path

failures: list[str] = []


def ok(message: str) -> None:
    print(f"ok    {message}", flush=True)


def bad(message: str, details: list[str] | None = None) -> None:
    failures.append(message)
    print(f"FAIL  {message}", flush=True)
    for line in (details or [])[:200]:
        print(f"        {line}", flush=True)
    if details and len(details) > 200:
        print(f"        ... and {len(details) - 200} more", flush=True)


def run(argv: list[str], timeout: float = 120, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, capture_output=True, text=True, timeout=timeout, env=env, check=False)


# Trees whose libraries are found another way than the system loader's search path, each
# checked where it is assembled: the signed runtime bundles and the browser engine resolve
# against their own directories (os/scripts/verify-runtime-bundle-closure.sh in the build),
# the preinstalled apps against their runtime, which the launcher mounts; kernel modules and
# firmware are no ELF programs; Windows and FEX payloads are for another loader.
LIBRARY_ROOTS = ["/usr/bin", "/usr/sbin", "/usr/libexec", "/usr/lib64", "/usr/lib"]
LIBRARY_SKIP = (
    "/usr/lib/modules/",
    "/usr/lib/firmware/",
    "/usr/lib/debug/",
    "/usr/lib/tilo-runtime/",
    "/usr/lib/sysimage/",
    "/usr/lib/tilo-apps/",
    "/usr/lib/fex-emu/",
    "/usr/lib64/wine/",
    "/usr/lib/wine/",
    "/usr/share/",
)
NATIVE_MACHINE = {"x86_64": 62, "aarch64": 183}[os.uname().machine]  # e_machine


def elf_kind(path: str) -> str | None:
    """`program` or `library` for a dynamically linked ELF of this machine, else None: ldd
    answers nothing useful for the rest. A program names its loader (PT_INTERP)."""
    try:
        with open(path, "rb") as file:
            head = file.read(64)
            if len(head) < 64 or head[:4] != b"\x7fELF" or head[4] != 2 or head[5] != 1:
                return None
            if int.from_bytes(head[18:20], "little") != NATIVE_MACHINE:
                return None
            phoff = int.from_bytes(head[32:40], "little")
            phentsize = int.from_bytes(head[54:56], "little")
            phnum = int.from_bytes(head[56:58], "little")
            file.seek(phoff)
            headers = file.read(phentsize * phnum)
    except OSError:
        return None
    types = {int.from_bytes(headers[i:i + 4], "little") for i in range(0, len(headers) - 3, phentsize)}
    if 2 not in types:  # PT_DYNAMIC: statically linked
        return None
    return "program" if 3 in types else "library"


def elf_files() -> dict[str, str]:
    seen: set[tuple[int, int]] = set()
    found = {}
    for root in LIBRARY_ROOTS:
        for directory, subdirs, files in os.walk(root):
            if (directory + "/").startswith(LIBRARY_SKIP):
                subdirs[:] = []
                continue
            for name in files:
                path = os.path.join(directory, name)
                try:
                    st = os.lstat(path)
                except OSError:
                    continue
                if not stat.S_ISREG(st.st_mode) or (st.st_dev, st.st_ino) in seen:
                    continue
                seen.add((st.st_dev, st.st_ino))
                kind = elf_kind(path)
                if kind:
                    found[path] = kind
    return found


def present_names() -> set[str]:
    """The name of every file and link in the image's library trees."""
    names = set()
    for root in ["/usr/lib64", "/usr/lib", "/usr/libexec", "/opt"]:
        for _directory, _subdirs, files in os.walk(root):
            names.update(files)
    return names


def missing_libraries(path: str) -> list[str]:
    result = run(["ldd", path], timeout=60)
    return sorted({line.split()[0] for line in result.stdout.splitlines() if "=> not found" in line})


def check_libraries() -> None:
    files = elf_files()
    if len(files) < 1000:
        bad(f"only {len(files)} ELF files under {', '.join(LIBRARY_ROOTS)}: not an OS image")
        return
    # A program resolves its libraries through the loader's search path and its own RUNPATH,
    # so ldd's answer for it is the one it gets. A library in a private directory (samba's,
    # systemd's) is resolved through the program that loads it, which ldd doesn't know: it
    # fails only when the library it needs is nowhere in the image.
    present = present_names()
    unresolved = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as pool:
        for (path, kind), missing in zip(files.items(), pool.map(missing_libraries, files)):
            if kind == "library":
                missing = [name for name in missing if name not in present]
            if missing:
                unresolved.append(f"{path}: {' '.join(missing)}")
    if unresolved:
        bad(f"{len(unresolved)} of {len(files)} ELF files link libraries the image lacks", unresolved)
    else:
        ok(f"all {len(files)} ELF files resolve their libraries")



def check_interpreters() -> None:
    broken = []
    count = 0
    path_dirs = ["/usr/bin", "/usr/sbin"]
    for root in ["/usr/bin", "/usr/sbin", "/usr/libexec"]:
        for directory, _subdirs, files in os.walk(root):
            for name in files:
                path = os.path.join(directory, name)
                if os.path.islink(path) or not os.access(path, os.X_OK):
                    continue
                try:
                    with open(path, "rb") as file:
                        first = file.readline(256)
                except OSError:
                    continue
                if not first.startswith(b"#!"):
                    continue
                count += 1
                words = first[2:].decode(errors="replace").split()
                if not words:
                    broken.append(f"{path}: empty #! line")
                    continue
                interpreter = words[0]
                if os.path.basename(interpreter) == "env":
                    program = next((w for w in words[1:] if not w.startswith("-")), None)
                    if program is None or not any(os.access(os.path.join(d, program), os.X_OK) for d in path_dirs):
                        broken.append(f"{path}: {' '.join(words)} (no {program} on PATH)")
                elif not os.access(interpreter, os.X_OK):
                    broken.append(f"{path}: {interpreter} is missing")
    if broken:
        bad(f"{len(broken)} of {count} scripts name an interpreter the image lacks", broken)
    else:
        ok(f"all {count} scripts' interpreters exist")


# The rootfs stage removes these with `rpm -e --nodeps` on purpose (os/Containerfile, the
# OpenSSL-backed plugins and Perl's HTTPS backend no Tilo path uses; os/image/packages.Containerfile,
# the build-time CMake macros and Ninja that Wine's mingw toolchain requires), so what required
# them is knowingly left without.
REMOVED_ON_PURPOSE = re.compile(
    r"^(libshout|perl\(IO::Socket::SSL\)|perl\(Net::SSLeay\)|perl-IO-Socket-SSL|perl-Net-SSLeay"
    r"|cmake-rpm-macros |ninja-build )"
)


def check_rpm_dependencies() -> None:
    result = run(["rpm", "-Va", "--nofiles", "--noscripts", "--nodigest", "--nosignature"], timeout=600)
    # rpm prints a header per package, then a line per requirement: `X is needed by (installed) P`.
    unsatisfied = [
        line.strip()
        for line in result.stdout.splitlines()
        if line.startswith((" ", "\t")) and line.strip() and not REMOVED_ON_PURPOSE.match(line.strip())
    ]
    listed = run(["rpm", "-qa"])
    count = len(listed.stdout.split())
    if listed.returncode != 0 or count < 100:
        bad(f"rpm lists {count} installed packages", (listed.stderr + result.stderr).splitlines())
        return
    if unsatisfied:
        bad(f"{len(unsatisfied)} dependencies of the {count} installed packages are not installed", unsatisfied)
    else:
        ok(f"the {count} installed packages' dependencies are installed")


UNIT_DIRS = {
    "system": ["/etc/systemd/system", "/usr/lib/systemd/system"],
    "user": ["/etc/systemd/user", "/usr/lib/systemd/user"],
}


def enabled_units(scope: str) -> tuple[set[str], list[str]]:
    """Units the image enables (its .wants/.requires links), and links to nothing."""
    units, dangling = set(), []
    for root in UNIT_DIRS[scope]:
        if not Path(root).is_dir():
            continue
        for link_dir in sorted([*Path(root).glob("*.wants"), *Path(root).glob("*.requires")]):
            for link in sorted(link_dir.iterdir()):
                name = link.name
                if "@" in name and not name.split("@", 1)[1].split(".")[0]:
                    continue
                template = re.sub(r"@[^.]+\.", "@.", name)
                if not any(Path(d, n).exists() for d in UNIT_DIRS[scope] for n in (name, template)):
                    dangling.append(f"{link} -> {os.readlink(link) if link.is_symlink() else name}")
                    continue
                units.add(name)
    return units, dangling


# systemd-analyze verify loads each unit with everything it pulls in; outside a booted system
# it can't tell what a few lines mean, which says nothing about the image.
VERIFY_NOISE = re.compile(
    r"(Failed to (open|connect|create|setup).*(bus|/run|/proc|cgroup)"
    r"|Cannot add dependency job"
    r"|is not a valid|Unit is bound to inactive unit"
    r"|Failed to load configuration: No such file"
    r"|Unknown key name '[^']+' in section '(X-[^']*)'"
    r"|System is tainted)"
)


def check_units() -> None:
    for scope in ("system", "user"):
        units, dangling = enabled_units(scope)
        if dangling:
            bad(f"{len(dangling)} {scope} units are enabled but not installed", dangling)
        if not units:
            bad(f"no {scope} units are enabled")
            continue
        paths = []
        for name in sorted(units):
            for directory in UNIT_DIRS[scope]:
                candidate = Path(directory, name)
                if not candidate.exists():
                    candidate = Path(directory, re.sub(r"@[^.]+\.", "@.", name))
                if candidate.exists():
                    paths.append(str(candidate))
                    break
        argv = ["systemd-analyze", f"--{scope}", "verify", "--man=no", "--generators=no", "--recursive-errors=no"]
        env = {**os.environ, "SYSTEMD_LOG_COLOR": "0", "XDG_RUNTIME_DIR": "/tmp"}
        result = run([*argv, *paths], timeout=600, env=env)
        problems = sorted({line for line in (result.stderr + result.stdout).splitlines() if line.strip() and not VERIFY_NOISE.search(line)})
        if problems:
            bad(f"systemd-analyze verify finds problems in the {len(paths)} enabled {scope} units", problems)
        else:
            ok(f"the {len(paths)} enabled {scope} units verify")


# The elements Tilo's own code builds pipelines from (crates/platform/tilo-media, the music,
# video and voice paths, recording, screen streaming) and the software path of the miracast
# crate tilo-projector runs on.
GST_ELEMENTS = [
    "playbin", "playbin3", "uridecodebin", "decodebin", "appsrc", "appsink", "autoaudiosink",
    "pulsesrc", "pulsesink", "volume", "capsfilter", "audioconvert", "audioresample",
    "videoconvert", "videoscale", "videorate", "opusenc", "opusdec", "vp9enc", "vp9dec",
    "av1enc", "dav1ddec", "qtmux", "mp4mux", "qtdemux", "h264parse", "hlssink2",
    "pipewiresrc", "mpegtsmux", "rtpmp2tpay", "udpsink", "aacparse", "avenc_aac",
]
# Miracast tries the hardware H.264 encoders first and falls back to these, in this order
# (the miracast crate's media.rs). The image carries no H.264 encoder of its own: they arrive
# with the media-codecs extension the owner downloads (os/usr/share/doc/tilo/codec-pack.md).
SOFTWARE_H264 = ["openh264enc", "x264enc"]
MIRACAST_AUDIO = (
    "audiotestsrc num-buffers=90 ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2"
    " ! avenc_aac ! aacparse ! mux."
)
MIRACAST_MUX = "mpegtsmux name=mux alignment=7 ! rtpmp2tpay ! fakesink sync=false"


def gst_env() -> dict[str, str]:
    registry = "/tmp/tilo-image-smoke-gst-registry.bin"
    return {**os.environ, "GST_REGISTRY": registry, "GST_REGISTRY_FORK": "no"}


def check_gstreamer() -> None:
    env = gst_env()
    blacklisted = run(["gst-inspect-1.0", "-b"], env=env, timeout=300)
    lines = [l.strip() for l in blacklisted.stdout.splitlines() if l.strip() and not l.startswith(("Blacklisted files:", "Total count:"))]
    if blacklisted.returncode != 0:
        bad("gst-inspect-1.0 -b fails", (blacklisted.stdout + blacklisted.stderr).splitlines())
    elif lines:
        bad(f"{len(lines)} GStreamer plugins fail to load", lines)
    else:
        ok("every GStreamer plugin loads")
    missing = [element for element in GST_ELEMENTS if run(["gst-inspect-1.0", "--exists", element], env=env).returncode != 0]
    if missing:
        bad(f"{len(missing)} GStreamer elements Tilo builds pipelines from are missing", missing)
    else:
        ok(f"all {len(GST_ELEMENTS)} GStreamer elements Tilo builds pipelines from exist")
    # Miracast's media path without its video: AAC muxed to MPEG-TS and sent as RTP.
    result = run(["gst-launch-1.0", "-q", *f"{MIRACAST_MUX} {MIRACAST_AUDIO}".split()], env=env, timeout=120)
    if result.returncode == 0:
        ok("Miracast's audio path runs: AAC in MPEG-TS over RTP")
    else:
        bad("Miracast's audio path fails: AAC in MPEG-TS over RTP", (result.stderr or result.stdout).splitlines()[-10:])
    installed = [e for e in SOFTWARE_H264 if run(["gst-inspect-1.0", "--exists", e], env=env).returncode == 0]
    if not installed:
        print(f"note  no software H.264 encoder ({', '.join(SOFTWARE_H264)}): Miracast's video needs"
              " the media-codecs extension or a hardware encoder", flush=True)
    for encoder in installed:
        video = (
            "videotestsrc num-buffers=60 ! video/x-raw,width=1280,height=720,framerate=30/1"
            f" ! videoconvert ! {encoder} ! h264parse ! mux."
        )
        result = run(["gst-launch-1.0", "-q", *f"{MIRACAST_MUX} {video} {MIRACAST_AUDIO}".split()], env=env, timeout=120)
        if result.returncode == 0:
            ok(f"Miracast's video path runs with {encoder}: H.264 and AAC in MPEG-TS over RTP")
        else:
            bad(f"Miracast's video path fails with {encoder}", (result.stderr or result.stdout).splitlines()[-10:])


# External programs the desktop starts, each with an argument that only prints and exits.
PROGRAMS = [
    ["Xwayland", "-version"],
    ["pipewire", "--version"],
    ["wireplumber", "--version"],
    ["NetworkManager", "--version"],
    ["bwrap", "--version"],
    ["passt", "--version"],
    ["podman", "--version"],
    ["bootc", "--version"],
    ["gst-launch-1.0", "--version"],
    ["swtpm", "--version"],
    [f"qemu-system-{os.uname().machine}", "--version"],
    ["tesseract", "--version"],
]


def check_programs() -> None:
    failed = []
    for argv in PROGRAMS:
        if shutil.which(argv[0]) is None:
            failed.append(f"{argv[0]}: not installed")
            continue
        try:
            result = run(argv, timeout=30)
        except subprocess.TimeoutExpired:
            failed.append(f"{' '.join(argv)}: did not exit in 30 s")
            continue
        if result.returncode != 0:
            failed.append(f"{' '.join(argv)}: exit {result.returncode}: {(result.stderr or result.stdout).strip()[:300]}")
    # xwayland-satellite has no version flag; with no compositor to join it must say so and
    # exit, which takes the program loading and starting.
    satellite = shutil.which("xwayland-satellite")
    if satellite is None:
        failed.append("xwayland-satellite: not installed")
    else:
        env = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "DISPLAY")}
        env["XDG_RUNTIME_DIR"] = "/tmp"
        try:
            result = run([satellite, ":99"], timeout=30, env=env)
            text = result.stderr + result.stdout
            if "compositor" not in text.lower():
                failed.append(f"xwayland-satellite: exit {result.returncode}: {text.strip()[:300]}")
        except subprocess.TimeoutExpired:
            failed.append("xwayland-satellite: did not exit in 30 s without a compositor")
    if failed:
        bad(f"{len(failed)} of {len(PROGRAMS) + 1} programs the desktop runs do not start", failed)
    else:
        ok(f"all {len(PROGRAMS) + 1} programs the desktop runs start")


def main() -> int:
    for check in (check_libraries, check_interpreters, check_rpm_dependencies, check_units, check_gstreamer, check_programs):
        try:
            check()
        except Exception as error:  # a check that can't run is a failure, never a pass
            bad(f"{check.__name__} could not run: {error!r}")
    if failures:
        print(f"\n{len(failures)} image smoke check(s) failed", flush=True)
        return 1
    print("\nimage smoke checks pass", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
