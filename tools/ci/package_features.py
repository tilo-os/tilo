#!/usr/bin/env python3
"""Feature selections for native package verification, never app deployment.

Gateway defaults are client-only. Its own tests must also observe the server,
trusted-host adapter and native segmentation rather than passing without them.
The Imager's default is its Tilo app; its and its backend's tests also cover
the installation.
Only enable these features when the gateway itself is selected, not for its
client dependents. Keep package argument strings separate from feature flags.
"""
from __future__ import annotations

import sys

NATIVE_TEST_FEATURES = {
    "tilo-model-gateway": ("local-models", "host-backend", "runtime-client"),
    # The Imager's default build is the Tilo app (the image writer); its tests
    # and lints cover the guided installation as the host downloads ship it.
    "imager": ("installation",),
    # Its Discord session reader (LevelDB, C++) is the installation's alone, so
    # the Tilo app does not link libstdc++; its tests and lints cover it.
    "imager-backend": ("installation",),
}


def arguments(packages: list[str]) -> list[str]:
    features = [f"{package}/{feature}" for package in sorted(set(packages))
                for feature in NATIVE_TEST_FEATURES.get(package, ())]
    return ["--features", ",".join(features)] if features else []


def main(argv: list[str]) -> None:
    packages = []
    for index, arg in enumerate(argv):
        if arg in ("-p", "--package"):
            packages.append(argv[index + 1])
        elif arg == "--workspace":
            packages.extend(NATIVE_TEST_FEATURES)
    print(" ".join(arguments(packages)))


if __name__ == "__main__":
    main(sys.argv[1:])
