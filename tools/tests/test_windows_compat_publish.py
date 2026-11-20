"""windows-compat-sysext.sh publish: a payload without SELinux labels is refused.

The check reads the image with debugfs, as os/dev/sysext-image.py does; here
debugfs answers as a tiny fixture, so no filesystem is created. Everything the
publisher would send to is a fake that logs what it was asked.
"""

import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
COMMIT = "a" * 40

USR = "system_u:object_r:usr_t:s0"
LIB = "system_u:object_r:lib_t:s0"
LABELS = {2: "system_u:object_r:root_t:s0", 11: "system_u:object_r:lost_found_t:s0",
          12: USR, 13: LIB, 14: LIB, 15: LIB}
LISTINGS = {
    2: ["/2/040755/0/0/.//", "/2/040755/0/0/..//", "/11/040700/0/0/lost+found//",
        "/12/040755/0/0/usr//"],
    11: ["/11/040700/0/0/.//", "/2/040755/0/0/..//"],
    12: ["/12/040755/0/0/.//", "/2/040755/0/0/..//", "/13/040755/0/0/lib//"],
    13: ["/13/040755/0/0/.//", "/12/040755/0/0/..//", "/14/040755/0/0/tilo//"],
    14: ["/14/040755/0/0/.//", "/13/040755/0/0/..//", "/15/100755/0/0/proton//"],
}

DEBUGFS = """#!/usr/bin/env python3
import sys
script = open(sys.argv[sys.argv.index('-f') + 1]).read().splitlines()
labels = {LABELS!r} if {labelled!r} else {{}}
listings = {LISTINGS!r}
for command in script:
    print('debugfs: ' + command, flush=True)
    ino = int(command.split('<')[1].split('>')[0])
    if command.startswith('ls -p'):
        print('\\n'.join(listings[ino]), flush=True)
    elif ino in labels:
        print(labels[ino], flush=True)
"""

CURL = """#!/usr/bin/env python3
import sys
open({log!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')
print('404' if '--head' in sys.argv else '200')
"""

UPLOAD = """#!/bin/sh
echo "$@" >> {log!r}
"""


class Publish(unittest.TestCase):
    def publish(self, labelled):
        directory = tempfile.TemporaryDirectory(prefix="windows-compat-publish-")
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        for name in ("windows-compat-sysext.sh", "lib.sh", "build-lib.sh"):
            target = root / "tools/ci/publish" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((REPO / "tools/ci/publish" / name).read_bytes())
        (root / "os/dev").mkdir(parents=True)
        (root / "os/dev/sysext-image.py").write_bytes(
            (REPO / "os/dev/sysext-image.py").read_bytes())
        log = root / "requests.log"
        upload_log = root / "uploads.log"
        binary = root / "bin"
        binary.mkdir()
        debugfs = binary / "debugfs"
        debugfs.write_text(DEBUGFS.format(LABELS=LABELS, labelled=labelled, LISTINGS=LISTINGS))
        binary.joinpath("curl").write_text(CURL.format(log=str(log)))
        binary.joinpath("dnf").write_text("#!/bin/sh\nexit 0\n")
        for program in ("debugfs", "curl", "dnf"):
            binary.joinpath(program).chmod(0o755)
        (root / "tools/upload_immutable_packages.sh").write_text(UPLOAD.format(log=str(upload_log)))
        self.build_layout(root / "windows-compat-sysext/oci")
        scratch = root / "scratch"
        scratch.mkdir()
        process = subprocess.Popen(
            ["bash", str(root / "tools/ci/publish/windows-compat-sysext.sh"), "publish", "x86_64",
             str(root)],
            cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            env={"PATH": f"{binary}:{os.environ['PATH']}", "TMPDIR": str(scratch),
                 "T_REF": "main", "T_COMMIT": COMMIT, "TILO_PACKAGES_TOKEN": "fixture"},
        )
        stdout, stderr = process.communicate()
        result = subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
        return result, (log.read_text() if log.exists() else ""), (
            upload_log.read_text() if upload_log.exists() else "")

    def build_layout(self, layout):
        raw = b"a small ext4 image\n"
        descriptor = json.dumps({"schema": 1, "part": "tilo-windows-compat",
                                 "file": "tilo-windows-compat.raw", "size": len(raw),
                                 "sha256": hashlib.sha256(raw).hexdigest(), "fs": "ext4"})
        tar_bytes = io.BytesIO()
        with tarfile.open(fileobj=tar_bytes, mode="w") as archive:
            for name, body in (("./descriptor.json", descriptor.encode()),
                               ("./tilo-windows-compat.raw", raw)):
                member = tarfile.TarInfo(name)
                member.size = len(body)
                archive.addfile(member, io.BytesIO(body))
        manifest = json.dumps({"layers": [{"digest": "sha256:" + hashlib.sha256(
            tar_bytes.getvalue()).hexdigest()}]})
        layout.mkdir(parents=True)
        (layout / "blobs/sha256").mkdir(parents=True)
        (layout / "index.json").write_text(json.dumps({"manifests": [{"digest": "sha256:" + hashlib.sha256(
            manifest.encode()).hexdigest()}]}))
        for body in (tar_bytes.getvalue(), manifest.encode()):
            (layout / "blobs/sha256" / hashlib.sha256(body).hexdigest()).write_bytes(body)

    def test_an_unlabelled_payload_is_refused_before_anything_is_sent(self):
        result, requests, uploads = self.publish(labelled=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("would change /usr", result.stderr)
        self.assertIn("has no SELinux label", result.stderr)
        self.assertEqual((requests, uploads), ("", ""), "nothing reached the package registry")

    def test_a_labelled_payload_is_published(self):
        result, requests, uploads = self.publish(labelled=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("tilo-windows-compat.raw", requests)
        self.assertEqual(len(uploads.strip().splitlines()), 2, uploads)


if __name__ == "__main__":
    unittest.main()
