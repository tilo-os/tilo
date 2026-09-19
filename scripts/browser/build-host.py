#!/usr/bin/env python3
"""Fetch the pinned CEF SDK and build the native Chromium host independently of Cargo."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
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


def download_archive(cache, entry):
    archive = cache / entry["name"]
    if verify_archive(archive, entry):
        return archive
    url = "https://cef-builds.spotifycdn.com/" + urllib.parse.quote(entry["name"])
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


def acquire_sdk(cache, entry):
    cache.mkdir(parents=True, exist_ok=True)
    name = entry["name"].removesuffix(".tar.bz2")
    sdk = cache / name
    marker = sdk / ".qos-verified-archive"
    if marker.is_file() and marker.read_text().strip() == entry["sha256"] and (sdk / "include/cef_app.h").is_file():
        return sdk
    archive = download_archive(cache, entry)
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch-only", action="store_true")
    parser.add_argument("--platform", choices=["macosarm64", "macosx64", "linux64", "linuxarm64"])
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 4, 8))
    args = parser.parse_args()
    key = args.platform or platform_key(platform.system(), platform.machine())
    lock = json.loads(Path(__file__).with_name("cef-lock.json").read_text())
    sdk = acquire_sdk(ROOT / "target/cef-sdk", lock["platforms"][key])
    print(f"CEF_ROOT={sdk}", flush=True)
    if args.fetch_only:
        return
    local_bin = ROOT / "target/chromium-tools/bin"
    cmake = str(local_bin / "cmake") if (local_bin / "cmake").is_file() else shutil.which("cmake")
    if not cmake:
        raise SystemExit("CMake is required. Install CMake, or use target/chromium-tools Python venv with cmake and ninja.")
    env = dict(os.environ, PATH=str(local_bin) + os.pathsep + os.environ["PATH"])
    subprocess.run(["cargo", "build", "-p", "qos-chromium-blocker", "--profile", "thin-app"], cwd=ROOT, check=True)
    blocker = ROOT / "target/thin-app/libqos_chromium_blocker.a"
    build = ROOT / "target/chromium-build"
    subprocess.run([cmake, "-S", str(ROOT / "apps/browser/chromium"), "-B", str(build),
                    f"-DCEF_ROOT={sdk}", f"-DQOS_BLOCKER_LIBRARY={blocker}", "-DCMAKE_BUILD_TYPE=Release",
                    f"-DQOS_OUTPUT_DIR={ROOT / 'target/chromium'}"], check=True, env=env)
    subprocess.run([cmake, "--build", str(build), "--parallel", str(args.jobs)], check=True, env=env)


if __name__ == "__main__":
    main()
