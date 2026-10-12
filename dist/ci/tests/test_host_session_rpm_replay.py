#!/usr/bin/env python3
"""Exercise source identity, archive bounds, cleanup and actual replay preflight."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch
import zipfile

CI = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("replay", CI / "fetch-host-session-rpms.py")
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)
SHA = "a" * 40
RUN = {"id": 123, "commit_sha": SHA, "workflow_id": "host-session-e2e.yml",
       "repository": {"full_name": "tilo/tilo"}, "status": "success", "index_in_repo": 456}
ARTIFACT = {"id": 789, "run_id": 123, "name": "fedora-44-amd64",
            "expired": False, "size_in_bytes": 1024}
JOBS = [{"name": "rpms", "status": "success"}]
NAMES = ["tilo-session-2026.07-1.x86_64.rpm", "tilo-session-apps-2026.07-1.noarch.rpm",
         "tilo-session-devel-2026.07-1.noarch.rpm"]


def archive(names=NAMES):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as bundle:
        for name in names:
            bundle.writestr(name, b"fixture RPM")
        bundle.writestr("unrelated-os-payload.tar.gz", b"never extracted")
    return buffer.getvalue()


class HostSessionRpmReplayTests(unittest.TestCase):
    def test_exact_source_and_unique_accepted_artifact_required(self):
        self.assertEqual(replay.select_artifact(RUN, [ARTIFACT], 123, SHA, JOBS), ARTIFACT)
        self.assertEqual(replay.select_artifact({**RUN, "status": "failure"}, [ARTIFACT],
                                               123, SHA, JOBS), ARTIFACT)
        for field, wrong in [("id", 999), ("commit_sha", "b" * 40),
                             ("workflow_id", "other.yml"),
                             ("repository", {"full_name": "other/repo"})]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                replay.select_artifact({**RUN, field: wrong}, [ARTIFACT], 123, SHA, JOBS)
        for artifacts in [[], [ARTIFACT, ARTIFACT], [{**ARTIFACT, "expired": True}],
                          [{**ARTIFACT, "name": "failed-fedora-44-amd64"}],
                          [{**ARTIFACT, "run_id": 999}],
                          [{**ARTIFACT, "size_in_bytes": replay.MAX_ARCHIVE + 1}]]:
            with self.subTest(artifacts=artifacts), self.assertRaises(ValueError):
                replay.select_artifact(RUN, artifacts, 123, SHA, JOBS)
        with self.assertRaises(ValueError):
            replay.select_artifact(RUN, [ARTIFACT], 123, SHA, [{"name": "rpms", "status": "failure"}])

    def test_unsafe_partial_and_extra_rpm_sets_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            for names in [NAMES[:2], NAMES + ["tilo-session-2.x86_64.rpm"],
                          ["../" + NAMES[0], *NAMES[1:]],
                          ["nested/" + NAMES[0], *NAMES[1:]],
                          ["tilo-session-unknown.rpm", *NAMES[1:]],
                          [NAMES[0], NAMES[1], "tilo-session-apps-2.noarch.rpm"]]:
                with self.subTest(names=names), self.assertRaises(ValueError):
                    replay.extract_rpms(io.BytesIO(archive(names)), Path(temporary))
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_replay_records_actual_build_and_harness_commits_without_credentials(self):
        data = archive()
        with tempfile.TemporaryDirectory() as temporary, \
                patch.dict(os.environ, GITHUB_SHA="b" * 40), \
                patch.object(replay, "metadata", side_effect=[RUN, [{**ARTIFACT, "size_in_bytes": len(data)}], JOBS]), \
                patch.object(replay.urllib.request, "urlopen", return_value=io.BytesIO(data)) as request:
            output = Path(temporary) / "rpms"
            replay.fetch("123", SHA, output)
            source = json.loads((output / "source.json").read_text())
            self.assertEqual(source["commit"], SHA)
            self.assertEqual(source["harness_commit"], "b" * 40)
            self.assertEqual(source["run_id"], 123)
            self.assertEqual(set(source["rpms"]), set(NAMES))
            self.assertEqual(source["artifact_purpose"], "host-session-acceptance-only")
            self.assertEqual(source["release_signing_claim"], "none")
            self.assertEqual(source["signature_verification"], "not-performed-by-this-source-report")
            self.assertFalse((output / "unrelated-os-payload.tar.gz").exists())
            request.assert_called_once_with(replay.API + "/actions/artifacts/789/zip", timeout=60)

    def test_incomplete_download_cleans_output_and_never_falls_back_to_compiling(self):
        data = archive()
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(replay, "metadata", side_effect=[RUN, [{**ARTIFACT, "size_in_bytes": len(data) + 1}], JOBS]), \
                patch.object(replay.urllib.request, "urlopen", return_value=io.BytesIO(data)):
            output = Path(temporary) / "rpms"
            with self.assertRaisesRegex(ValueError, "incomplete"):
                replay.fetch("123", SHA, output)
            self.assertFalse(output.exists())
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_actual_workflow_report_does_not_turn_an_artifact_name_into_authority(self):
        workflow = (CI.parents[1] / ".forgejo/workflows/host-session-e2e.yml").read_text()
        block = workflow.split("- name: Record this run's RPMs", 1)[1]
        script = textwrap.dedent(block.split("        run: |\n", 1)[1].split("      - name:", 1)[0])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'rpms').mkdir()
            for name in NAMES:
                (root / 'rpms' / name).write_bytes(b'unsigned development fixture')
            result = subprocess.run(['bash', '-c', script], capture_output=True,
                                    env=dict(os.environ, WORK=str(root), RUN_NUMBER='456', RUN_ID='123',
                                             WORKFLOW='host-session-e2e', GITHUB_SHA=SHA,
                                             GITHUB_REF_NAME='topic', ARTIFACT='signed-fedora-44-amd64'))
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            source = json.loads((root / 'rpms/source.json').read_text())
            self.assertEqual(source['artifact'], 'signed-fedora-44-amd64')
            self.assertEqual(source['release_signing_claim'], 'none')
            self.assertEqual(source['signature_verification'], 'not-performed-by-this-source-report')

    def test_actual_workflow_preflight_rejects_partial_or_invalid_replay_inputs(self):
        workflow = (CI.parents[1] / ".forgejo/workflows/host-session-e2e.yml").read_text()
        block = workflow.split("- name: Validate explicit RPM replay source", 1)[1]
        script = textwrap.dedent(block.split("        run: |\n", 1)[1].split("      # Fail before checkout", 1)[0])
        unset = {k: v for k, v in os.environ.items() if k not in ["RPM_SOURCE_RUN", "RPM_SOURCE_COMMIT"]}
        self.assertEqual(subprocess.run(["bash", "-c", script], env=unset, capture_output=True).returncode, 0)
        for run, commit, accepted in [("", "", True), ("123", SHA, True), ("123", "", False),
                                      ("", SHA, False), ("0", SHA, False), ("../123", SHA, False),
                                      ("123", SHA.upper(), False), ("123", "a" * 39, False)]:
            with self.subTest(run=run, commit=commit):
                result = subprocess.run(["bash", "-c", script], capture_output=True,
                                        env=dict(os.environ, RPM_SOURCE_RUN=run, RPM_SOURCE_COMMIT=commit))
                self.assertEqual(result.returncode == 0, accepted)
        rpms = workflow.split("  rpms:\n", 1)[1].split("  host-session:\n", 1)[0]
        self.assertEqual(rpms.count("if: ${{ !inputs.rpm-artifact && !inputs.rpm-source-run }}"), 6)


if __name__ == "__main__":
    unittest.main()
