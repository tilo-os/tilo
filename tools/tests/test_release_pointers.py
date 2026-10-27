#!/usr/bin/env python3
"""The app-collection and desktop-payload pointers, as their publishers write them and readers read them."""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci/publish"))
import pointers  # noqa: E402

BASE = "https://code.tilo.so/api/packages/tilo/generic"
COMMIT = "c" * 40


def collection_pointer(**changes) -> dict:
    value = {"schema": 1, "collection": "default",
             "x86_64": {"url": f"{BASE}/tilo-app-collections/0.0.0-{COMMIT}-{'a' * 64}/tilo-apps-x86_64.tar.gz",
                        "sha256": "a" * 64},
             "aarch64": {"url": f"{BASE}/tilo-app-collections/0.0.0-{COMMIT}-{'b' * 64}/tilo-apps-aarch64.tar.gz",
                         "sha256": "b" * 64},
             "builder": "registry.tilo.so/tilo/tilo-builder@sha256:" + "d" * 64, "source": COMMIT}
    value.update(changes)
    return value


def cli(*args, environ=None, timeout=HANG_BOUND):
    return subprocess.run([sys.executable, "-I", str(ROOT / "tools/ci/publish/pointers.py"), *args],
                          capture_output=True, text=True, env=environ, timeout=timeout)


class PublishedPointers(unittest.TestCase):
    """What the publishing scripts write is what the reader accepts."""

    def test_the_app_channel_publish_apps_writes_is_an_app_collection_pointer(self):
        script = (ROOT / "tools/apps/publish-collections.sh").read_text()
        # The publisher's own lines: the jq that writes the channel file t sets the pointer to.
        block = re.search(r'\n(    jq -n --arg url_x86_64 .*?> "\$TILO_APP_CHANNEL_OUT")\n', script, re.S)
        self.assertIsNotNone(block, "publish-collections.sh no longer writes the channel with jq -n")
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / "channel.json"
            x86 = collection_pointer()["x86_64"]
            for arm, complete in ((collection_pointer()["aarch64"], "true"), (None, "false")):
                with self.subTest(aarch64=arm is not None, complete=complete):
                    environ = dict(os.environ, channel_url=x86["url"], channel_sha=x86["sha256"],
                                   arm64_channel_url=arm["url"] if arm else "",
                                   arm64_channel_sha=arm["sha256"] if arm else "",
                                   BUILDER_IMAGE="registry.tilo.so/tilo/tilo-builder@sha256:" + "d" * 64,
                                   COMPLETE=complete, GITHUB_SHA=COMMIT, TILO_APP_CHANNEL_OUT=str(out))
                    subprocess.run(["bash", "-euo", "pipefail", "-c", block.group(1)], env=environ, check=True,
                                   timeout=HANG_BOUND)
                    pointer = pointers.read("app-collection", out)
                    self.assertEqual(pointers.entry(pointer, "x86_64"), x86)
                    self.assertEqual(pointer["aarch64"], arm)
                    self.assertEqual(pointer["source"], COMMIT if complete == "true" else None)

    def test_the_payload_publish_desktop_payload_writes_is_a_desktop_payload_pointer(self):
        with tempfile.TemporaryDirectory() as temp:
            tree = Path(temp) / "tree"
            for name in ("dist/ci/publish-desktop-payload.sh", "tools/ci/publish/lib.sh"):
                (tree / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / name, tree / name)
            lane = tree / "dist/ci-output/fedora-44-amd64"
            lane.mkdir(parents=True)
            (lane / "tilo-desktop-os-x86_64.tar.gz").write_bytes(b"payload")
            digest = subprocess.run(["sha256sum", "tilo-desktop-os-x86_64.tar.gz"], cwd=lane, check=True,
                                    capture_output=True, text=True, timeout=HANG_BOUND).stdout
            (lane / "tilo-desktop-os-x86_64.tar.gz.sha256").write_text(digest)
            bin_dir = Path(temp) / "bin"
            bin_dir.mkdir()
            # The package registry answers every upload with 201.
            (bin_dir / "curl").write_text("#!/bin/sh\ncat >/dev/null\nprintf 201\n")
            (bin_dir / "curl").chmod(0o755)
            out = Path(temp) / "payload.json"
            environ = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}", T_REF="stable",
                           T_COMMIT=COMMIT, REGISTRY_TOKEN="fixture", TMPDIR=temp)
            result = subprocess.run(["bash", str(tree / "dist/ci/publish-desktop-payload.sh"), str(out)],
                                    env=environ, capture_output=True, text=True, timeout=HANG_BOUND)
            self.assertEqual(result.returncode, 0, result.stderr)
            pointer = pointers.read("desktop-payload", out)
            self.assertEqual(pointer["source"], COMMIT)
            self.assertEqual(pointers.entry(pointer, "x86_64"), {
                "url": f"{BASE}/tilo-desktop-os/0.0.0-{COMMIT}/tilo-desktop-os-x86_64.tar.gz",
                "sha256": digest.split()[0]})
            with self.assertRaises(pointers.Unset):
                pointers.entry(pointer, "aarch64")


class Reading(unittest.TestCase):
    def test_a_pointer_is_read_from_its_variable_or_a_file(self):
        text = json.dumps(collection_pointer())
        self.assertEqual(pointers.variable("app-collection"), "T_POINTER_APP_COLLECTION")
        self.assertEqual(pointers.read("app-collection", environ={"T_POINTER_APP_COLLECTION": text})["source"],
                         COMMIT)
        for environ in ({}, {"T_POINTER_APP_COLLECTION": ""}, {"T_POINTER_APP_COLLECTION": " \n"}):
            with self.assertRaises(pointers.Unset):
                pointers.read("app-collection", environ=environ)
        with self.assertRaises(pointers.Unset):
            pointers.read("app-collection", Path("/nonexistent/channel.json"))

    def test_digests_are_normalized_and_unknown_members_ignored(self):
        value = collection_pointer(publisher="e" * 64)
        value["x86_64"]["sha256"] = "A" * 64
        pointer = pointers.parse("app-collection", json.dumps(value))
        self.assertEqual(pointer["x86_64"]["sha256"], "a" * 64)
        self.assertNotIn("publisher", pointer)

    def test_what_is_not_a_pointer_is_refused(self):
        def refused(name, value):
            with self.subTest(name=name, value=value), self.assertRaises(pointers.PointerError):
                pointers.parse(name, value if isinstance(value, str) else json.dumps(value))

        refused("app-collection", "not json")
        refused("app-collection", collection_pointer(schema=2))
        refused("app-collection", collection_pointer(collection="extra"))
        refused("app-collection", collection_pointer(x86_64=None))
        refused("app-collection", collection_pointer(builder=""))
        refused("app-collection", collection_pointer(source="main"))
        refused("kernel", collection_pointer())
        for url in ("http://code.tilo.so/api/packages/tilo/generic/a/b/c.tar.gz",
                    "https://code.tilo.so.example/api/packages/tilo/generic/a/b/c.tar.gz",
                    "https://code.tilo.so/api/packages/other/generic/a/b/c.tar.gz",
                    f"{BASE}/a/../b/c.tar.gz", f"{BASE}/a/b/c.tar.gz?x=1", f"{BASE}/a/b/c/d.tar.gz",
                    "http://forgejo.forgejo.svc.cluster.local:3000/api/packages/tilo/generic/a/b/c.tar.gz"):
            refused("app-collection", collection_pointer(x86_64={"url": url, "sha256": "a" * 64}))
        refused("app-collection", collection_pointer(aarch64={"url": f"{BASE}/a/b/c", "sha256": "short"}))
        payload = {"schema": 1, "x86_64": None, "aarch64": None, "source": COMMIT}
        refused("desktop-payload", payload)
        refused("desktop-payload", dict(payload, x86_64={"url": f"{BASE}/a/b/c", "sha256": "a" * 64}, source=None))
        pointers.parse("desktop-payload", json.dumps(dict(payload, x86_64={"url": f"{BASE}/a/b/c", "sha256": "a" * 64})))

    def test_the_command_line_prints_fields_and_says_what_is_missing(self):
        with tempfile.TemporaryDirectory() as temp:
            file = Path(temp) / "channel.json"
            file.write_text(json.dumps(collection_pointer(aarch64=None)))
            got = cli("get", "app-collection", "url", "--arch", "x86_64", "--file", str(file))
            self.assertEqual((got.returncode, got.stdout.strip()), (0, collection_pointer()["x86_64"]["url"]))
            self.assertEqual(cli("get", "app-collection", "source", "--file", str(file)).stdout.strip(), COMMIT)
            missing = cli("get", "app-collection", "sha256", "--arch", "aarch64", "--file", str(file))
            self.assertEqual(missing.returncode, 3, missing.stderr)
            self.assertIn("nothing for aarch64", missing.stderr)
            shown = cli("show", "app-collection", environ=dict(os.environ, T_POINTER_APP_COLLECTION=file.read_text()))
            self.assertEqual(json.loads(shown.stdout)["builder"], collection_pointer()["builder"])
            unset = cli("show", "desktop-payload", environ={k: v for k, v in os.environ.items()
                                                            if k != "T_POINTER_DESKTOP_PAYLOAD"})
            self.assertEqual(unset.returncode, 3)
            file.write_text(json.dumps(collection_pointer(schema=0)))
            self.assertEqual(cli("show", "app-collection", "--file", str(file)).returncode, 1)


if __name__ == "__main__":
    unittest.main()
