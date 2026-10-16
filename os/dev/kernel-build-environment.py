#!/usr/bin/env python3
"""Print the signed build environment a Tilo kernel's provenance binds.

Reads `cosign verify-attestation --type slsaprovenance1` output for one
tilo-kernel image on stdin. It selects the kernel-rpm/v1 predicate for the
given manifest digest and architecture, with a tilo/tilo source commit, and then
prints the one digest-pinned build environment that predicate names, the
signed native kernel buildroot:

  registry.tilo.so/tilo/tilo-kernel-buildroot@sha256:...

The caller must still verify the printed reference's signature with the
release key. The environment is deliberately not compared with the OS image's
own RPM snapshot or base: they are not kernel inputs.

Exit status 1 means no single matching predicate or build environment.
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import sys

BUILD_TYPE = "https://tilo.sh/buildtypes/kernel-rpm/v1"
SOURCE = re.compile(r"^git\+ssh://code\.tilo\.so/tilo/tilo@[0-9a-f]{40}$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
BUILDROOT = re.compile(r"^registry\.tilo\.so/tilo/tilo-kernel-buildroot@sha256:[0-9a-f]{64}$")


def documents(text: str):
    decoder = json.JSONDecoder()
    index = 0
    while True:
        while index < len(text) and text[index].isspace():
            index += 1
        if index >= len(text):
            return
        value, index = decoder.raw_decode(text, index)
        yield from value if isinstance(value, list) else (value,)


def predicates(text: str):
    for envelope in documents(text):
        try:
            statement = json.loads(base64.b64decode(envelope["payload"]))
        except (KeyError, TypeError, ValueError):
            continue
        predicate = statement.get("predicate")
        if isinstance(predicate, dict):
            yield predicate


def environment(text: str, digest: str, architecture: str) -> str | None:
    found = set()
    for predicate in predicates(text):
        definition = predicate.get("buildDefinition") or {}
        if (definition.get("buildType") != BUILD_TYPE
                or (definition.get("externalParameters") or {}).get("architecture") != architecture
                or (definition.get("internalParameters") or {}).get("expectedReleaseDigest") != digest):
            continue
        uris = [dependency.get("uri") for dependency in definition.get("resolvedDependencies") or []
                if isinstance(dependency, dict) and isinstance(dependency.get("uri"), str)]
        if not any(SOURCE.fullmatch(uri) for uri in uris):
            continue
        buildroots = {uri for uri in uris if BUILDROOT.fullmatch(uri)}
        # A matching predicate must be usable on its own. Ignoring an ambiguous
        # one lets a second, valid predicate hide the conflict.
        if len(buildroots) != 1:
            return None
        found |= buildroots
    return found.pop() if len(found) == 1 else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--digest", required=True, help="the kernel manifest digest")
    parser.add_argument("--architecture", required=True, choices=("x86_64", "aarch64"))
    args = parser.parse_args()
    if not DIGEST.fullmatch(args.digest):
        parser.error("--digest must be a sha256 digest")
    try:
        ref = environment(sys.stdin.read(), args.digest, args.architecture)
    except ValueError:
        ref = None
    if ref is None:
        return 1
    print(ref)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
