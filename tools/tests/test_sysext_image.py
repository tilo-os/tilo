"""tools/extensions/sysext_image.py: the manifest of a prebuilt sysext image."""

import datetime
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/extensions"))
import resolve  # noqa: E402
import sysext_image  # noqa: E402

DESCRIPTOR = {"schema": 1, "part": "tilo-windows-compat", "file": "tilo-windows-compat.raw",
              "size": 1_900_000_000, "sha256": "a" * 64, "fs": "erofs"}
URL = "https://code.tilo.so/api/packages/tilo/generic/tilo-windows-compat-image/aarch64-abc/tilo-windows-compat.raw"
IMAGE = "registry.tilo.so/tilo/tilo@sha256:" + "b" * 64


def manifest(**overrides):
    arguments = dict(ident="windows-compat", descriptor=DESCRIPTOR, url=URL, fingerprint="c" * 64,
                     image=IMAGE, arch="aarch64", os_id="fedora", version_id="44", publisher="Tilo",
                     license_text="see image",
                     now=datetime.datetime(2026, 10, 10, 12, 0, 0, tzinfo=datetime.timezone.utc))
    arguments.update(overrides)
    return sysext_image.build_manifest(**arguments)


class SysextManifest(unittest.TestCase):
    def test_one_pinned_artifact_for_one_base(self):
        value = manifest()
        self.assertEqual(resolve.check_manifest(value), [])
        (part,) = value["parts"]
        self.assertEqual((part["format"], part["state"]), ("sysext-raw", "available"))
        (artifact,) = part["artifacts"]
        self.assertEqual((artifact["size"], artifact["sha256"], artifact["urls"]),
                         (1_900_000_000, "a" * 64, [URL]))
        self.assertEqual(value["revision"], 20261010120000)
        self.assertEqual(value["base"]["packages_sha256"], "c" * 64)

    def test_refuses_a_descriptor_that_does_not_pin_the_file(self):
        for broken in ({"sha256": "nope"}, {"size": 0}, {"file": "other.raw"}, {"part": "Bad Part"}):
            with self.assertRaises(resolve.ResolveError, msg=broken):
                manifest(descriptor={**DESCRIPTOR, **broken})

    def test_refuses_plain_http_or_a_url_for_another_file(self):
        for url in (URL.replace("https", "http"), URL.replace("tilo-windows-compat.raw", "x.raw")):
            with self.assertRaises(resolve.ResolveError):
                manifest(url=url)

    def test_check_refuses_two_artifacts_in_one_image_part(self):
        value = manifest()
        value["parts"][0]["artifacts"].append(dict(value["parts"][0]["artifacts"][0]))
        self.assertTrue(any("exactly one artifact" in p for p in resolve.check_manifest(value)))

    def test_a_withdrawn_payload_is_offered_as_unsupported(self):
        reason = "Not offered for this version of Tilo: it has no SELinux labels."
        value = manifest(withdrawn={"a" * 64: reason})
        self.assertEqual(resolve.check_manifest(value), [])
        (part,) = value["parts"]
        self.assertEqual((part["state"], part["message"], part["artifacts"]),
                         ("unsupported", reason, []))
        self.assertNotIn(URL, json.dumps(value), "a withdrawn payload is not named to devices")

    def test_another_payloads_withdrawal_leaves_this_one_offered(self):
        value = manifest(withdrawn={"d" * 64: "withdrawn"})
        self.assertEqual(value["parts"][0]["state"], "available")

    def test_the_shipped_withdrawals_are_readable(self):
        withdrawn = sysext_image.withdrawn_payloads()
        self.assertIn("56717954c7f34ff7fd93f5c78e761a2283c65dbc1fea81f6cdc84a41bb06741a",
                      withdrawn)
        self.assertTrue(all(reason for reason in withdrawn.values()))

    def test_a_malformed_withdrawal_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "withdrawn.toml"
            path.write_text('schema = 1\n[[payload]]\nsha256 = "nope"\nreason = "x"\n')
            with self.assertRaises(resolve.ResolveError):
                sysext_image.withdrawn_payloads(path)

    def test_command_line_writes_a_manifest_resolve_check_accepts(self):
        with tempfile.TemporaryDirectory() as tmp:
            descriptor = Path(tmp) / "descriptor.json"
            descriptor.write_text(json.dumps(DESCRIPTOR))
            withdrawn = Path(tmp) / "withdrawn.toml"
            withdrawn.write_text('schema = 1\n[[payload]]\nsha256 = "' + "a" * 64
                                 + '"\nreason = "withdrawn for this test"\n')
            out = Path(tmp) / "windows-compat.json"
            command = [sys.executable, "-I", str(REPO / "tools/extensions/sysext_image.py"), "manifest",
                       "--id", "windows-compat", "--descriptor", str(descriptor), "--url", URL,
                       "--fingerprint", "c" * 64, "--image", IMAGE, "--arch", "aarch64",
                       "--withdrawn", str(withdrawn), "--out", str(out)]
            subprocess.check_call(command)
            self.assertEqual(json.loads(out.read_text())["parts"][0]["state"], "unsupported")
            command.remove("--withdrawn")
            command.remove(str(withdrawn))
            subprocess.check_call(command)
            checked = subprocess.run([sys.executable, "-I", str(REPO / "tools/extensions/resolve.py"),
                                      "check", str(out)], check=True, capture_output=True, text=True)
            self.assertTrue(checked.stdout.startswith("windows-compat aarch64 " + "c" * 64))
            self.assertEqual(hashlib.sha256(out.read_bytes()).hexdigest(),
                             hashlib.sha256(resolve.render(json.loads(out.read_text()))).hexdigest())


if __name__ == "__main__":
    unittest.main()
