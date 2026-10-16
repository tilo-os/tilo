"""Build the pinned SDK MoQ module once and retain its licenses in each bundle."""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "crates/browser/tilo-browser-runtime/web"
MODULE = "media-moq-library.mjs"
LICENSE = "media-moq-library.LICENSE.txt"


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").digest()


def prepare():
    builder = ROOT / "scripts/moq-browser"
    subprocess.run(["npm", "ci", "--no-audit", "--no-fund", "--silent"], cwd=builder, check=True)
    subprocess.run(["node", "build.mjs"], cwd=builder, check=True)
    # A successful command with missing or stale output is not a usable build.
    if (ASSETS / MODULE).stat().st_mtime_ns < (builder / "package-lock.json").stat().st_mtime_ns:
        raise ValueError("MoQ output predates the pinned lock")
    digest(ASSETS / MODULE)
    digest(ASSETS / LICENSE)


def copy_license(bundle):
    # The production web target keeps its lock-freshness check. Refuse to add
    # licenses to a different module rather than silently replacing that build.
    if digest(bundle / MODULE) != digest(ASSETS / MODULE):
        raise ValueError("bundle MoQ module differs from the pinned shared build")
    destination = bundle / LICENSE
    shutil.copyfile(ASSETS / LICENSE, destination)
    if digest(destination) != digest(ASSETS / LICENSE):
        raise ValueError("bundle MoQ license copy differs")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("prepare", "license"))
    parser.add_argument("bundle", nargs="?", type=Path)
    args = parser.parse_args()
    if args.mode == "prepare":
        if args.bundle is not None:
            parser.error("prepare does not take a bundle")
        prepare()
    else:
        if args.bundle is None:
            parser.error("license requires the actual built bundle")
        copy_license(args.bundle)


if __name__ == "__main__":
    main()
