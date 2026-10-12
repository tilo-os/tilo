#!/usr/bin/env python3
"""Verify a reviewed Desktop catalog once, then consume its exact-source artifact.

`prepare` regenerates the checked-in catalog with build-desktop-catalog.py.
Nonempty catalogs require a reviewed manifest and the exact published package
bytes; no native evidence claims are inferred from build or CI success.
`consume` binds the prepared catalog bytes to this workflow's source commit.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener


SPEC = importlib.util.spec_from_file_location("desktop_catalog_generator", Path(__file__).with_name("build-desktop-catalog.py"))
generator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(generator)


def load(path):
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=generator.reject_duplicate_fields)


def revision(value):
    generator.require(re.fullmatch(r"[0-9a-f]{40}", value or "") is not None, "A full workflow source revision is required")
    return value


class PackageRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        target = urlsplit(new_url)
        generator.require(target.scheme == "https" and target.hostname is not None
                          and target.username is None and target.password is None,
                          "Package redirect must remain HTTPS without URL credentials")
        redirected = super().redirect_request(request, response, code, message, headers, new_url)
        if redirected is not None and (target.hostname != "code.tilo.so" or target.port not in (None, 443)):
            for source in (redirected.headers, redirected.unredirected_hdrs):
                for key in list(source):
                    if key.lower() == "authorization":
                        del source[key]
        return redirected


def fetch_package(release, expected, destination):
    generator.trusted_url(release["url"])
    size = expected.get("byte_size")
    generator.require(type(size) is int and 0 < size <= 2**63 - 1, "Catalog package size is invalid")
    generator.require(release["verified_runtime"]["package_sha256"] == expected.get("sha256"),
                      "Reviewed package digest differs from the catalog")
    headers = {"Accept": "application/octet-stream"}
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = "token " + token
    request = Request(release["url"], headers=headers)
    count = 0
    digest = hashlib.sha256()
    with build_opener(PackageRedirect()).open(request, timeout=60) as response, destination.open("wb") as output:
        while block := response.read(min(1024 * 1024, size - count + 1)):
            count += len(block)
            generator.require(count <= size, "Published package exceeds its reviewed size")
            digest.update(block)
            output.write(block)
    generator.require(count == size and digest.hexdigest() == expected["sha256"],
                      "Published package does not match its reviewed digest and size")


def prepare(catalog_path, manifest_path, output, source, fetch=False):
    revision(source)
    raw = catalog_path.read_bytes()
    catalog = json.loads(raw.decode("utf-8"), object_pairs_hook=generator.reject_duplicate_fields)
    generator.fields(catalog, {"schema", "tag", "releases"}, "Catalog")
    generator.require(type(catalog["schema"]) is int and catalog["schema"] == 1, "Unsupported catalog schema")
    generator.require(isinstance(catalog["releases"], list), "Catalog releases must be a list")
    if not catalog["releases"]:
        generated = generator.build(catalog, catalog_path.parent)
    else:
        generator.require(manifest_path is not None, "A nonempty Desktop catalog requires its reviewed release manifest")
        generator.require(manifest_path.is_file(), "A nonempty Desktop catalog requires its reviewed release manifest")
        manifest = load(manifest_path)
        generator.fields(manifest, {"schema", "tag", "releases"}, "Manifest")
        with tempfile.TemporaryDirectory(prefix="tilo-desktop-catalog-") as directory:
            if fetch:
                expected = {release["id"]: release for release in catalog["releases"]}
                releases = []
                for index, release in enumerate(manifest["releases"]):
                    generator.require(release["id"] in expected, "Reviewed release is absent from the catalog")
                    artifact = Path(directory) / f"package-{index}"
                    fetch_package(release, expected[release["id"]], artifact)
                    releases.append({**release, "artifact": str(artifact)})
                manifest = {**manifest, "releases": releases}
            generated = generator.build(manifest, manifest_path.parent)
    generator.require(json.dumps(generated, sort_keys=True) == json.dumps(catalog, sort_keys=True),
                      "Embedded Desktop catalog differs from its reviewed packages and native evidence")
    output.mkdir(parents=True, exist_ok=True)
    (output / "desktop-catalog.json").write_bytes(raw)
    (output / "desktop-catalog-receipt.json").write_text(json.dumps({
        "schema": 1, "source_revision": source, "catalog_sha256": hashlib.sha256(raw).hexdigest()
    }, sort_keys=True) + "\n", encoding="utf-8")


def consume(directory, source, destination):
    revision(source)
    receipt = load(directory / "desktop-catalog-receipt.json")
    generator.fields(receipt, {"schema", "source_revision", "catalog_sha256"}, "Catalog receipt")
    generator.require(type(receipt["schema"]) is int and receipt["schema"] == 1
                      and receipt["source_revision"] == source, "Desktop catalog receipt is for another workflow source")
    catalog = directory / "desktop-catalog.json"
    raw = catalog.read_bytes()
    generator.require(hashlib.sha256(raw).hexdigest() == receipt["catalog_sha256"],
                      "Desktop catalog artifact differs from its verified receipt")
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepared = commands.add_parser("prepare")
    prepared.add_argument("--catalog", required=True, type=Path)
    prepared.add_argument("--review-manifest", type=Path)
    prepared.add_argument("--output", required=True, type=Path)
    prepared.add_argument("--source", required=True)
    prepared.add_argument("--fetch", action="store_true")
    consumed = commands.add_parser("consume")
    consumed.add_argument("directory", type=Path)
    consumed.add_argument("--source", required=True)
    consumed.add_argument("--copy-to", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args.catalog, args.review_manifest, args.output, args.source, args.fetch)
        else:
            consume(args.directory, args.source, args.copy_to)
    except (OSError, ValueError, TypeError, KeyError) as error:
        # Network exceptions may contain a presigned redirect URL. Report the
        # category only; validation failures have deliberately fixed messages.
        message = str(error) if type(error) is ValueError else type(error).__name__
        parser.exit(1, f"Desktop catalog release refused: {message}\n")


if __name__ == "__main__":
    main()
