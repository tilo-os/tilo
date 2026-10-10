#!/usr/bin/python3
"""Write the unsigned manifest of a prebuilt sysext image (format "sysext-raw").

    sysext_image.py manifest --id windows-compat --descriptor DESCRIPTOR.json \\
        --url https://code.tilo.so/.../tilo-windows-compat.raw \\
        --fingerprint HEX64 --image REF@sha256:... --arch aarch64 \\
        [--os-id fedora --version-id 44] --out windows-compat.json

DESCRIPTOR.json is what os/dev/build-windows-compat-sysext.sh printed when it
built the image ({"part","file","size","sha256","fs"}). The manifest pins that
one file by URL, exact length and SHA-256 for exactly one base (the image's
package fingerprint, `resolve.py fingerprint` run inside the OS image). It goes
through the same `resolve.py check`, signing (publish.sh, the extension-manifest
KMS key) and device verification (os/usr/libexec/tilo-extension) as an
RPM-assembled extension; only the assembly differs: the device links the
verified image into /var/lib/extensions instead of unpacking packages.
"""

from __future__ import annotations

import argparse
import datetime
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import resolve  # noqa: E402

PART_RE = re.compile(r"[a-z0-9][a-z0-9-]{0,62}")


def build_manifest(*, ident: str, descriptor: dict, url: str, fingerprint: str, image: str,
                   arch: str, os_id: str, version_id: str, publisher: str, license_text: str,
                   now: datetime.datetime | None = None) -> dict:
    part = descriptor.get("part")
    if not isinstance(part, str) or not PART_RE.fullmatch(part):
        raise resolve.ResolveError("the descriptor names no valid part")
    if descriptor.get("file") != f"{part}.raw":
        raise resolve.ResolveError("the descriptor's file must be <part>.raw")
    if not url.startswith("https://") or not url.endswith(f"/{part}.raw"):
        raise resolve.ResolveError("the image URL must be HTTPS and end in /<part>.raw")
    size, sha256 = descriptor.get("size"), descriptor.get("sha256")
    if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
        raise resolve.ResolveError("the descriptor has no size")
    if not isinstance(sha256, str) or not resolve.HEX64.fullmatch(sha256):
        raise resolve.ResolveError("the descriptor has no sha256")
    moment = (now or datetime.datetime.now(datetime.timezone.utc)).replace(microsecond=0)
    manifest = {
        "type": resolve.MANIFEST_TYPE,
        "schema": resolve.MANIFEST_SCHEMA,
        "id": ident,
        "revision": int(moment.strftime("%Y%m%d%H%M%S")),
        "created": moment.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "base": {
            "os_id": os_id,
            "version_id": version_id,
            "architecture": arch,
            "packages_sha256": fingerprint,
            "image": image,
        },
        "parts": [{
            "name": part,
            "format": resolve.RAW_FORMAT,
            "state": "available",
            "message": "",
            "requires_base": {},
            "artifacts": [{
                "package": f"{part}.raw",
                "urls": [url],
                "size": size,
                "sha256": sha256,
                "publisher": publisher,
                "license": license_text,
            }],
        }],
    }
    problems = resolve.check_manifest(manifest)
    if problems:
        raise resolve.ResolveError("; ".join(problems))
    return manifest


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("action", choices=("manifest",))
    parser.add_argument("--id", required=True)
    parser.add_argument("--descriptor", type=Path, required=True)
    parser.add_argument("--url", required=True)
    parser.add_argument("--fingerprint", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--arch", required=True, choices=resolve.ARCHITECTURES)
    parser.add_argument("--os-id", default="fedora")
    parser.add_argument("--version-id", default="44")
    parser.add_argument("--publisher", default="Tilo")
    parser.add_argument("--license", default="Contents carry their own licenses under /usr/share/licenses")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        manifest = build_manifest(
            ident=args.id, descriptor=json.loads(args.descriptor.read_text()), url=args.url,
            fingerprint=args.fingerprint, image=args.image, arch=args.arch, os_id=args.os_id,
            version_id=args.version_id, publisher=args.publisher, license_text=args.license)
    except (resolve.ResolveError, OSError, ValueError) as error:
        print(f"sysext_image: {error}", file=sys.stderr)
        return 1
    args.out.write_bytes(resolve.render(manifest))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
