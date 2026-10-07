#!/usr/bin/env python3
"""Publish staged, receipt-checked speech objects to exact immutable package URLs.

Used only by the existing protected publisher. Refuse redirects, distinguish
404 from authentication/server failures, and verify anonymous readback before
the catalog compiler signs. The token is read from the environment, never an
argument or diagnostic. Candidate review needs no publication credentials.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import urllib.error
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("immutable package must not redirect")


def verify(response, size, digest):
    observed = hashlib.sha256()
    count = 0
    while chunk := response.read(min(65536, size + 1 - count)):
        count += len(chunk)
        if count > size:
            raise ValueError("published object exceeds reviewed size")
        observed.update(chunk)
    if count != size or observed.hexdigest() != digest:
        raise ValueError("immutable package receipt mismatch")


def publish(path, url, size, digest, token):
    # Both source staging and this publisher must enforce the same origin.
    if not url.startswith("https://code.tilo.so/api/packages/tilo/generic/whisper-"):
        raise ValueError("unapproved package destination")
    if not path.is_file() or path.is_symlink() or path.stat().st_size != size:
        raise ValueError("staged artifact is not a reviewed regular file")
    with path.open("rb") as stream:
        verify(stream, size, digest)
    opener = urllib.request.build_opener(NoRedirect())
    try:
        with opener.open(url, timeout=180) as response:
            verify(response, size, digest)
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise ValueError(f"package read failed with HTTP {error.code}") from None
        if not token:
            raise ValueError("existing package publication credential unavailable")
        with path.open("rb") as stream:
            request = urllib.request.Request(url, data=stream, method="PUT", headers={
                "Authorization": f"token {token}", "Content-Length": str(size),
                "Content-Type": "application/octet-stream",
            })
            try:
                with opener.open(request, timeout=300) as response:
                    if response.status not in (200, 201, 204):
                        raise ValueError("unexpected package publication response")
            except urllib.error.HTTPError as error:
                raise ValueError(f"package publication failed with HTTP {error.code}") from None
    # A receipt conflict must fail; never overwrite an existing object.
    with opener.open(url, timeout=180) as response:
        verify(response, size, digest)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    source = json.loads((args.directory / "source.json").read_text())
    expected = ["whisper-tiny-multilingual", "whisper-base-multilingual", "whisper-small-q5_1-multilingual"]
    if [model["id"] for model in source["models"][1:]] != expected:
        raise ValueError("unexpected staged speech catalog")
    token = os.environ.get("PACKAGE_TOKEN", "")
    root = Path(__file__).resolve().parents[2]
    for model, package in zip(source["models"][1:], ["whisper-tiny", "whisper-base", "whisper-small-q5_1"]):
        receipt = json.loads((root / f"dist/local-models/{package}/upstream-receipt.json").read_text())
        base = f"https://code.tilo.so/api/packages/tilo/generic/{package}/{receipt['revision']}"
        artifact = model["artifact"]
        if (artifact != {"file": f"artifacts/{receipt['file']}", "url": f"{base}/{receipt['file']}",
                         "size_bytes": receipt["size_bytes"], "sha256": receipt["sha256"]}
                or model["license_url"] != f"{base}/LICENSE"):
            raise ValueError("staged catalog destination disagrees with reviewed receipt")
        publish(args.directory / artifact["file"], artifact["url"], receipt["size_bytes"], receipt["sha256"], token)
        publish(args.directory / f"artifacts/{package}-LICENSE", model["license_url"],
                receipt["license_size_bytes"], receipt["license_sha256"], token)
    print("Reviewed speech artifacts and complete MIT notices passed anonymous receipt readback.")


if __name__ == "__main__":
    main()
