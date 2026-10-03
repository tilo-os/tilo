#!/usr/bin/env python3
"""Package the browser, shared runtime launcher, and pinned Chromium runtime."""
import argparse
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=["dev", "thin-app", "release"], default="thin-app")
    parser.add_argument("--skip-native-build", action="store_true", help="Use an already built native runtime")
    args = parser.parse_args()
    if not args.skip_native_build:
        subprocess.run([sys.executable, str(ROOT / "scripts/browser/build-host.py")], check=True)
    subprocess.run(["cargo", "build", "-p", "browser", "--profile", args.profile], cwd=ROOT, check=True)
    # A package runs outside the Tilo desktop: it carries the standalone runtime.
    subprocess.run([
        "cargo", "build", "-p", "tilo-runtime", "--bin", "tilo-runtime",
        "--profile", args.profile,
    ], cwd=ROOT, check=True)
    binaries = ROOT / "target" / ("debug" if args.profile == "dev" else args.profile)
    binary = binaries / "browser"
    runtime_host = binaries / "tilo-runtime"
    output = ROOT / "target/browser-package"
    output.mkdir(parents=True, exist_ok=True)
    if platform.system() == "Darwin":
        runtime = ROOT / "target/chromium/Browser Engine.app"
        if not (runtime / "Contents/MacOS/tilo-chromium-host").is_file():
            raise SystemExit("Native runtime missing; run scripts/browser/build-host.py first")
        bundle = output / "Browser.app"
        contents = bundle / "Contents"
        (contents / "MacOS").mkdir(parents=True, exist_ok=True)
        frameworks = contents / "Frameworks"
        frameworks.mkdir(exist_ok=True)
        destination = frameworks / runtime.name
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(runtime, destination, symlinks=True)
        shutil.copy2(binary, contents / "MacOS/browser")
        bundled_host = contents / "MacOS/tilo-runtime"
        shutil.copy2(runtime_host, bundled_host)
        info = {
            "CFBundleIdentifier": "dev.tilo.browser",
            "CFBundleName": "Tilo Browser",
            "CFBundleDisplayName": "Tilo Browser",
            "CFBundleExecutable": "browser",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": "0.1.0",
            "CFBundleVersion": "1",
            "NSHighResolutionCapable": True,
            "NSCameraUsageDescription": "Allow websites you approve to use your camera.",
            "NSMicrophoneUsageDescription": "Allow websites you approve to use your microphone.",
        }
        with (contents / "Info.plist").open("wb") as stream:
            plistlib.dump(info, stream)
        subprocess.run(["codesign", "--force", "--sign", "-", str(bundled_host)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(bundle)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(bundle)], check=True)
        print(bundle)
    elif platform.system() == "Linux":
        runtime = ROOT / "target/chromium"
        if not (runtime / "tilo-chromium-host").is_file():
            raise SystemExit("Native runtime missing; run scripts/browser/build-host.py first")
        destination = output / "tilo-browser"
        destination.mkdir(exist_ok=True)
        engine = destination / "chromium"
        if engine.exists():
            shutil.rmtree(engine)
        shutil.copytree(runtime, engine, symlinks=True)
        shutil.copy2(binary, destination / "browser")
        shutil.copy2(runtime_host, destination / "tilo-runtime")
        print(destination / "browser")
    else:
        raise SystemExit("Only macOS and Linux are supported")


if __name__ == "__main__":
    main()
