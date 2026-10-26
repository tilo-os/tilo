#!/usr/bin/env python3
"""Feature selections for native package verification, never app deployment.

Gateway defaults are client-only. Its own tests must also observe the server,
trusted-host adapter and native segmentation rather than passing without them.
The Imager's default is its Tilo app; its and its backend's tests also cover
the installation. tilo-media builds without GStreamer by default; its own
decode, encode, real-time and voice tests compile only with those features, and
wait for a pipeline's start-up with the shared test bound only under
test-support, so its runs enable all five (the job image carries GStreamer).
A cargo family's build of named packages gets them through workspace-unify
(tools/ci/workspace_unify.py), so every package is built with them, the
gateway's client dependents too: what ships without them is checked by
DEFAULT_BUILDS_ALSO_CHECKED. Keep package argument strings separate from
feature flags.
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
    "tilo-media": ("decode", "encode", "realtime", "voice", "test-support"),
    # The fetch proxy a workspace host serves is a feature its relay enables;
    # tilo-net's own tests of it (`proxy_host`) and the development program
    # build only with it. Its loopback tests of every backend (`desktop`) need
    # the other client features; the public-server ones (`live`) stay ignored.
    "tilo-net": ("proxy-dev-server", "json", "ws", "webtransport", "sockets"),
    # Its relays, clients and their real-WebTransport tests are behind
    # `relay`; without it the package's own tests are the voice codec alone.
    "tilo-moq": ("relay",),
    # The listener browsers and the fetch proxy reach a host through, and their
    # tests, are behind `web`, which every deployed relay enables. (`agents`
    # links every web app, so its tests run in the agent checks of t.toml.)
    "tilo-relay": ("web",),
    "tilo-relay-shell": ("web",),
    # The rest compile their tests only with a feature their default build
    # leaves to the dependents that want it; each costs a few small crates.
    "wire-frame": ("tokio", "postcard"),
    "accounts": ("person",),
    "bounded-zip": ("epub",),
    "settings-store": ("watch",),
    "tilo-files": ("json",),
    "tilo-dav": ("test-support",),
    "tilo-crypto": ("srp-test-server",),
    "tilo-resource": ("test-support",),
    # Its model of a provider (per-field storage, the CRDT edits two devices
    # make at once) is a feature only the settings hosts enable; its own
    # tests of it compile only with `model`.
    "agent-provider-config": ("model",),
    "app-host": ("test-support",),
    "package-store": ("store",),
    "cloud-source": ("conformance",),
    "browser-continuity": ("native",),
    # Its librespot session and GStreamer sink, and every test of them, are
    # the `native` engine Music links; without it the package has no tests.
    "tilo-spotify": ("native",),
    # Reading an AppImage's embedded entry is the compositor's alone; its
    # tests build the images they read.
    "tilo-desktop-entry": ("appimage",),
    # The tunnel the shell links is a feature the root helper leaves out; its
    # tests, and the namespace test of both together, need it.
    "tilo-device-vpn": ("test-support",),
}

# Packages whose tested features above are not their default build. The
# default build is what ships (the Imager's Tilo app), so clippy also checks it
# on its own, and the `default-builds` check in t.toml builds its tests as it
# ships, each alone: otherwise code and tests the default leaves out rot
# unseen, as the Imager's did when the installation became a feature. Not in
# the test family's shared jobs, whose memory t budgets for one build each.
# Those whose tested features are a server's (the gateway's inference and host
# backend, tilo-net's development proxy, tilo-moq's relay, tilo-crypto's test
# SRP server) ship to clients without them; every other build has them
# (workspace-unify), so code that compiles only with them would pass unseen.
DEFAULT_BUILDS_ALSO_CHECKED = (
    "imager",
    "imager-backend",
    "tilo-crypto",
    "tilo-model-gateway",
    "tilo-moq",
    "tilo-net",
)


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
