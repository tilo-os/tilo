#!/usr/bin/env python3
"""The developer CI gate must bind image bytes to a real successful producer."""
import hashlib
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "dev/windows-direct-dev-provenance.py"
SPEC = importlib.util.spec_from_file_location("dev_ci", SCRIPT)
dev = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(dev)


class DevCITests(unittest.TestCase):
    def fixture(self, directory, profile="lenovo-83ed"):
        config = {"architecture": "arm64", "os": "linux", "config": {"Labels": {
            "sh.tilo.image.flavour": "dev", "org.tilo.with-windows": "1",
            "org.opencontainers.image.revision": "a" * 40}}}
        config_bytes = json.dumps(config).encode()
        config_path = Path(directory) / "config.json"
        config_path.write_bytes(config_bytes)
        manifest = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "config": {"digest": "sha256:" + hashlib.sha256(config_bytes).hexdigest(),
                               "size": len(config_bytes)},
                    "layers": [{"digest": "sha256:" + "b" * 64, "size": 42}]}
        manifest_bytes = json.dumps(manifest).encode()
        manifest_path = Path(directory) / "manifest.json"
        manifest_path.write_bytes(manifest_bytes)
        digest = "sha256:" + hashlib.sha256(manifest_bytes).hexdigest()
        args = SimpleNamespace(image=dev.REPOSITORY + "@" + digest, ci_run_id="17", profile=profile,
                               manifest=manifest_path, config=config_path)
        run = {"id": 17, "workflow_id": "os-dev.yml", "prettyref": "main",
               "is_fork_pull_request": False, "commit_sha": "a" * 40,
               "status": "failure", "index_in_repo": 100}
        jobs = [{"id": 1, "name": "build-and-push (aarch64, runner-large-arm64)",
                 "run_id": 17, "attempt": 1, "status": "success"},
                {"id": 2, "name": "board: lenovo-83ed (lenovo-83ed)",
                 "run_id": 17, "attempt": 1, "status": "success"},
                {"id": 3, "name": "board: unrelated", "run_id": 17, "status": "failure"}]
        suffix = "lenovo-83ed" if profile == "lenovo-83ed" else "ci"
        origin = dev.REPOSITORY + ":main-aaaaaaa-w17-" + suffix
        producer = ("exporting manifest " + digest + "\n"
                    + ("sealed image pushed (" + origin + "@" + digest + ")" if profile == "lenovo-83ed"
                       else "pushed " + origin + "-aarch64 (" + digest + ")")).encode()
        logs = {jobs[0]["name"]: producer, jobs[1]["name"]: ("Trying to pull " + args.image + "...").encode()}
        def fetch(*arguments):
            if arguments == ("api", "repos/tilo/tilo/actions/runs/17"):
                return json.dumps(run).encode()
            if arguments == ("api", "repos/tilo/tilo/actions/runs/17/jobs"):
                return json.dumps(jobs).encode()
            self.assertEqual(arguments[:4], ("actions", "run", "view", "100"))
            self.assertEqual(arguments[-3:], ("--log", "-R", "tilo/tilo"))
            return logs[arguments[5]]
        return args, run, jobs, logs, fetch

    def test_verified_producer_and_digest_bound_config(self):
        for profile in ("generic", "lenovo-83ed"):
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as directory:
                args, _, _, _, fetch = self.fixture(directory, profile)
                with patch.object(dev, "forgejo", side_effect=fetch):
                    evidence = dev.verify_image(args)
                self.assertEqual(evidence["authority"], "live-forgejo-development-run")
                self.assertEqual(evidence["source_revision"], "a" * 40)
                self.assertEqual(evidence["image"], args.image)
                self.assertEqual(evidence["image_kind"], profile)
                self.assertNotIn("key_sha256", evidence)

    def test_ci_metadata_and_job_refusals(self):
        mutations = (lambda run, jobs: run.update(commit_sha="c" * 40),
                     lambda run, jobs: run.update(prettyref="untrusted-branch"),
                     lambda run, jobs: run.update(is_fork_pull_request=True),
                     lambda run, jobs: jobs[0].update(status="failure"),
                     lambda run, jobs: jobs[1].update(status="failure"),
                     lambda run, jobs: jobs.append(dict(jobs[1])))
        for index, mutate in enumerate(mutations):
            with self.subTest(case=index), tempfile.TemporaryDirectory() as directory:
                args, run, jobs, _, fetch = self.fixture(directory)
                mutate(run, jobs)
                with patch.object(dev, "forgejo", side_effect=fetch), self.assertRaises(ValueError):
                    dev.verify_image(args)

    def test_image_tamper_and_unpublished_digest_refusals(self):
        with tempfile.TemporaryDirectory() as directory:
            args, _, jobs, logs, fetch = self.fixture(directory)
            args.config.write_bytes(args.config.read_bytes() + b" ")
            with patch.object(dev, "forgejo") as external, self.assertRaisesRegex(ValueError, "config"):
                dev.verify_image(args)
            external.assert_not_called()
        for name in ("build-and-push (aarch64, runner-large-arm64)", "board: lenovo-83ed (lenovo-83ed)"):
            with self.subTest(missing_log=name), tempfile.TemporaryDirectory() as directory:
                args, _, _, logs, fetch = self.fixture(directory)
                logs[name] = b"some unrelated successful build"
                with patch.object(dev, "forgejo", side_effect=fetch), self.assertRaises(ValueError):
                    dev.verify_image(args)


if __name__ == "__main__":
    unittest.main()
