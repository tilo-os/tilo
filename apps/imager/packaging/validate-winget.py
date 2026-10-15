#!/usr/bin/env python3
"""Validate a WinGet manifest set against WinGet's published JSON schemas.

`winget validate` needs Windows; this checks the same schemas on Linux (field
names, types, patterns, enums), which is what a malformed manifest usually
gets wrong. winget's own semantic checks (installer reachability, duplicate
architectures) still need Windows.

The schemas are downloaded from microsoft/winget-cli at a pinned commit and
checked against pinned digests, so the result never depends on what upstream
publishes next.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys
import urllib.request

import jsonschema
import yaml

COMMIT = "3d9083c062c1e7a7086e05becb414d4e005e4ab6"
VERSION = "1.9.0"
SCHEMAS = {
    "installer": "053cab82b5130bbf54ce97142ea852ad17e04fe59b1edafcd087c1254b327b90",
    "version": "6874e71495b38d13909192c18256ee9b63753d4eb4490253ff7e734ef3d1c4a5",
    "defaultLocale": "5c7dc375a3c3b95aede4a74dd0ec7ca090306938f14a8dcc5e346fe347934bea",
}


def schema(kind):
    url = (f"https://raw.githubusercontent.com/microsoft/winget-cli/{COMMIT}"
           f"/schemas/JSON/manifests/v{VERSION}/manifest.{kind}.{VERSION}.json")
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != SCHEMAS[kind]:
        raise ValueError(f"{url} does not have the pinned digest")
    return json.loads(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("manifests", type=Path, help="directory of one manifest set")
    args = parser.parse_args()
    seen = set()
    errors = []
    for path in sorted(args.manifests.glob("*.yaml")):
        document = yaml.safe_load(path.read_text(encoding="utf-8"))
        kind = document.get("ManifestType")
        if kind not in SCHEMAS:
            errors.append(f"{path.name}: unknown ManifestType {kind!r}")
            continue
        if document.get("ManifestVersion") != VERSION:
            errors.append(f"{path.name}: ManifestVersion is not {VERSION}")
        seen.add(kind)
        validator = jsonschema.Draft7Validator(schema(kind))
        for error in validator.iter_errors(document):
            where = "/".join(map(str, error.absolute_path)) or "(root)"
            errors.append(f"{path.name}: {where}: {error.message}")
    for kind in sorted(set(SCHEMAS) - seen):
        errors.append(f"no {kind} manifest in {args.manifests}")
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        sys.exit(1)
    print(f"{args.manifests}: {len(seen)} manifests valid against WinGet {VERSION} schemas")


if __name__ == "__main__":
    main()
