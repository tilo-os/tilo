#!/usr/bin/env python3
"""Fetch the pinned CEF SDK and build the native Chromium host independently of Cargo.

A plain run builds target/chromium/ for development (point TILO_CHROMIUM_HOST
at target/chromium/tilo-chromium-host). `--runtime DIR` also assembles the
engine runtime tree, DIR/chromium/, which image builds sign as the
`tilo-runtime/<version>` bundle the browser's manifest names
(ui::webview::chromium::runtime). `--sdk-archive` builds offline from an
already downloaded, still digest-checked SDK archive.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def platform_key(system, machine):
    arch = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x64", "AMD64": "x64"}.get(machine)
    if system == "Darwin" and arch:
        return "macos" + arch
    if system == "Linux" and arch:
        return "linux64" if arch == "x64" else "linuxarm64"
    raise ValueError(f"Unsupported Chromium host platform: {system}/{machine}")


def verify_archive(path, entry):
    if not path.is_file() or path.stat().st_size != entry["size"]:
        return False
    expected_sha256 = entry.get("sha256")
    if not isinstance(expected_sha256, str) or len(expected_sha256) != 64:
        raise ValueError("CEF lock entry requires a 64-digit SHA-256 digest")
    try:
        bytes.fromhex(expected_sha256)
    except ValueError as error:
        raise ValueError("CEF lock entry has an invalid SHA-256 digest") from error
    with path.open("rb") as stream:
        if hashlib.file_digest(stream, "sha256").hexdigest() != expected_sha256:
            return False
    # Retain the upstream index's SHA-1 as a secondary metadata cross-check;
    # SHA-256 is the mandatory content-integrity boundary above.
    if "sha1" in entry:
        with path.open("rb") as stream:
            return hashlib.file_digest(stream, "sha1").hexdigest() == entry["sha1"]
    return True


def valid_range(header, first, last, size, received):
    return header == f"bytes {first}-{last}/{size}" and received == last - first + 1


def archive_url(entry):
    if "url" not in entry:
        if "source_patch_sha256" in entry:
            raise ValueError("Rebuilt CEF lock requires its immutable SDK URL")
        return "https://cef-builds.spotifycdn.com/" + urllib.parse.quote(entry["name"])
    url = entry["url"]
    if not isinstance(url, str):
        raise ValueError("CEF lock URL must be a string")
    parsed = urllib.parse.urlsplit(url)
    source = entry.get("source_patch_sha256")
    if not isinstance(source, str) or not re.fullmatch("[0-9a-f]{64}", source):
        raise ValueError("Rebuilt CEF URL requires the locked source patch SHA-256")
    expected = "/api/packages/tilo/generic/tilo-cef-sdk/" + source + "/" + urllib.parse.quote(entry["name"], safe="")
    if (parsed.scheme != "https" or parsed.netloc != "code.tilo.so"
            or parsed.path != expected or parsed.query or parsed.fragment):
        raise ValueError("Rebuilt CEF URL must name the immutable locked Tilo SDK package")
    return url


def download_archive(cache, entry):
    url = archive_url(entry)
    archive = cache / entry["name"]
    if verify_archive(archive, entry):
        return archive
    parts = cache / (entry["name"] + ".parts")
    parts.mkdir(parents=True, exist_ok=True)
    chunk = 1024 * 1024

    def fetch(first):
        last = min(first + chunk, entry["size"]) - 1
        dest = parts / str(first)
        if dest.exists() and dest.stat().st_size == last - first + 1:
            return dest
        for attempt in range(3):
            try:
                req = urllib.request.Request(url, headers={"Range": f"bytes={first}-{last}"})
                with urllib.request.urlopen(req, timeout=90) as response:
                    data = response.read(chunk + 1)
                    if not valid_range(response.headers.get("Content-Range"), first, last, entry["size"], len(data)):
                        raise ValueError(f"Invalid SDK range response for {first}-{last}")
                temp = dest.with_suffix(".tmp")
                temp.write_bytes(data)
                temp.replace(dest)
                return dest
            except (OSError, ValueError):
                if attempt == 2:
                    raise

    print(f"Downloading pinned {entry['name']} ({entry['size'] // 1024**2} MiB)", flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
        ordered = list(pool.map(fetch, range(0, entry["size"], chunk)))
    temporary = archive.with_suffix(".download")
    with temporary.open("wb") as output:
        for part in ordered:
            with part.open("rb") as source:
                shutil.copyfileobj(source, output)
    if not verify_archive(temporary, entry):
        temporary.unlink(missing_ok=True)
        shutil.rmtree(parts)
        raise ValueError("CEF distribution checksum mismatch; discarded partial download")
    temporary.replace(archive)
    shutil.rmtree(parts)
    return archive


def acquire_sdk(cache, entry, archive=None):
    archive_url(entry)
    cache.mkdir(parents=True, exist_ok=True)
    name = entry["name"].removesuffix(".tar.bz2")
    sdk = cache / name
    marker = sdk / ".tilo-verified-archive"
    if marker.is_file() and marker.read_text().strip() == entry["sha256"] and (sdk / "include/cef_app.h").is_file():
        return sdk
    if archive is None:
        archive = download_archive(cache, entry)
    elif not verify_archive(archive, entry):
        raise ValueError(f"{archive} is not the pinned CEF distribution")
    print("Extracting verified CEF SDK", flush=True)
    with tempfile.TemporaryDirectory(prefix="cef-extract-", dir=cache) as temporary:
        with tarfile.open(archive) as source:
            source.extractall(temporary, filter="data")
        extracted = Path(temporary) / name
        if not (extracted / "include/cef_app.h").is_file():
            raise ValueError("CEF archive has unexpected layout")
        if sdk.exists():
            raise ValueError(f"Unverified SDK directory exists: {sdk}; move it aside before rebuilding")
        extracted.rename(sdk)
    marker.write_text(entry["sha256"] + "\n")
    return sdk


# What the engine needs at run time, beside the host: CEF's library, its
# graphics fallbacks and its resources. The setuid `chrome-sandbox` helper is
# left out: the host's renderers are confined through user namespaces.
RUNTIME_FILES = (
    "tilo-chromium-host",
    "libcef.so",
    "libEGL.so",
    "libGLESv2.so",
    "libvk_swiftshader.so",
    "libvulkan.so.1",
    "vk_swiftshader_icd.json",
    "v8_context_snapshot.bin",
    "icudtl.dat",
    "resources.pak",
    "chrome_100_percent.pak",
    "chrome_200_percent.pak",
)
RUNTIME_DIRS = ("locales",)


def is_elf(path):
    with path.open("rb") as stream:
        return stream.read(4) == b"\x7fELF"


def install_runtime(build, dest, strip=None):
    """Copy the engine's run-time files from `build` into `dest`/chromium.

    Fails when any is missing, so a CEF update that renames one is caught
    here and not on a device. ELF files are stripped when `strip` names the
    tool: CEF's library carries over a gigabyte of symbols.
    """
    engine = dest / "chromium"
    if engine.exists():
        shutil.rmtree(engine)
    engine.mkdir(parents=True)
    missing = [name for name in RUNTIME_FILES + RUNTIME_DIRS if not (build / name).exists()]
    if missing:
        raise ValueError(f"engine build lacks {', '.join(missing)}")
    for name in RUNTIME_FILES:
        target = engine / name
        shutil.copy2(build / name, target)
        if strip and is_elf(target):
            subprocess.run([strip, "--strip-unneeded", str(target)], check=True)
    for name in RUNTIME_DIRS:
        shutil.copytree(build / name, engine / name)
    return engine


def build_blocker_library():
    built = subprocess.run(
        ["cargo", "build", "--locked", "-p", "tilo-chromium-blocker", "--profile", "thin-app",
         "--message-format=json-render-diagnostics"],
        cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True,
    )
    for line in built.stdout.splitlines():
        message = json.loads(line)
        target = message.get("target", {})
        if (message.get("reason") == "compiler-artifact"
                and target.get("name") == "tilo_chromium_blocker"
                and "staticlib" in target.get("crate_types", [])):
            for filename in message.get("filenames", []):
                artifact = Path(filename)
                if artifact.suffix == ".a" and artifact.is_file():
                    return artifact
    raise ValueError("Cargo did not report the built Chromium blocker static library")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch-only", action="store_true")
    parser.add_argument("--platform", choices=["macosarm64", "macosx64", "linux64", "linuxarm64"])
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 4, 8))
    parser.add_argument("--sdk-archive", type=Path, help="Pinned CEF archive already on disk (no download)")
    parser.add_argument("--runtime", type=Path, help="Also assemble the engine runtime tree under this directory")
    args = parser.parse_args()
    key = args.platform or platform_key(platform.system(), platform.machine())
    lock = json.loads(Path(__file__).with_name("cef-lock.json").read_text())
    sdk = acquire_sdk(ROOT / "target/cef-sdk", lock["platforms"][key], args.sdk_archive)
    print(f"CEF_ROOT={sdk}", flush=True)
    if args.fetch_only:
        return
    local_bin = ROOT / "target/chromium-tools/bin"
    cmake = str(local_bin / "cmake") if (local_bin / "cmake").is_file() else shutil.which("cmake")
    if not cmake:
        raise SystemExit("CMake is required. Install CMake, or use target/chromium-tools Python venv with cmake and ninja.")
    env = dict(os.environ, PATH=str(local_bin) + os.pathsep + os.environ["PATH"])
    blocker = build_blocker_library()
    build = ROOT / "target/chromium-build"
    subprocess.run([cmake, "-S", str(ROOT / "apps/browser/chromium"), "-B", str(build),
                    f"-DCEF_ROOT={sdk}", f"-DTILO_BLOCKER_LIBRARY={blocker}", "-DCMAKE_BUILD_TYPE=Release",
                    f"-DTILO_OUTPUT_DIR={ROOT / 'target/chromium'}"], check=True, env=env)
    subprocess.run([cmake, "--build", str(build), "--parallel", str(args.jobs)], check=True, env=env)
    if args.runtime:
        engine = install_runtime(ROOT / "target/chromium", args.runtime, shutil.which("strip"))
        print(f"Engine runtime tree: {engine}", flush=True)


if __name__ == "__main__":
    main()
