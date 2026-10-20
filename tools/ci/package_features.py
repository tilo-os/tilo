#!/usr/bin/env python3
"""Feature selections for native package verification, never app deployment.

Gateway defaults are client-only. Its own tests must also observe the server,
trusted-host adapter and native segmentation rather than passing without them.
The Imager's default is its Tilo app; its and its backend's tests also cover
the installation. tilo-media builds without GStreamer by default; its own
decode, encode and real-time tests compile only with those features, and wait
for a pipeline's start-up with the shared test bound only under test-support,
so its runs enable all four (the job image carries GStreamer).
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
    "tilo-media": ("decode", "encode", "realtime", "test-support"),
}

# Packages whose tested features above are not their default build. The
# default build is what ships (the Imager's Tilo app), so clippy also checks it
# on its own, and the `default-builds` check in t.toml builds its tests as it
# ships, each alone: otherwise code and tests the default leaves out rot
# unseen, as the Imager's did when the installation became a feature. Not in
# the test family's shared jobs, whose memory t budgets for one build each.
DEFAULT_BUILDS_ALSO_CHECKED = ("imager", "imager-backend")


def arguments(packages: list[str]) -> list[str]:
    features = [f"{package}/{feature}" for package in sorted(set(packages))
                for feature in NATIVE_TEST_FEATURES.get(package, ())]
    return ["--features", ",".join(features)] if features else []


def default_builds(packages: list[str]) -> list[str]:
    """The selected packages whose default build clippy checks separately."""
    return sorted(set(packages) & set(DEFAULT_BUILDS_ALSO_CHECKED))


def main(argv: list[str]) -> None:
    if argv == ["--default-builds", "--all"]:
        # Every such package, for the `default-builds` check in t.toml.
        print(" ".join(sorted(DEFAULT_BUILDS_ALSO_CHECKED)))
        return
    defaults = bool(argv) and argv[0] == "--default-builds"
    if defaults:
        argv = argv[1:]
    packages = []
    for index, arg in enumerate(argv):
        if arg in ("-p", "--package"):
            packages.append(argv[index + 1])
        elif arg == "--workspace" and not defaults:
            packages.extend(NATIVE_TEST_FEATURES)
    print(" ".join(default_builds(packages) if defaults else arguments(packages)))


if __name__ == "__main__":
    main(sys.argv[1:])
