from pathlib import Path
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/build-image.yml"


def matrix_script() -> str:
    workflow = WORKFLOW.read_text()
    marker = "      - name: Select validation or native publication lanes\n"
    block = workflow.split(marker, 1)[1].split("\n\n  signing-preflight:", 1)[0]
    return textwrap.dedent(block.split("        run: |\n", 1)[1])


SHA = "0123456789abcdef0123456789abcdef01234567"


def plan(
    ref: str,
    event: str,
    requested: str,
    published: str = "",
    configured: str = "true",
    integration: str = "",
) -> tuple[bool, bool, list[dict[str, str]]]:
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / "output"
        environment = os.environ.copy()
        environment.update(
            {
                "GITHUB_REF": ref,
                "GITHUB_EVENT_NAME": event,
                "GITHUB_OUTPUT": str(output),
                "GITHUB_SHA": SHA,
                "REQUESTED_PUBLISH": requested,
                "PUBLISHED_REVISION": published,
                "PUBLICATION_CONFIGURED": configured,
                "INTEGRATION_REVISION": integration,
            }
        )
        subprocess.run(["bash", "-c", matrix_script()], env=environment, check=True)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        return (
            values["publish"] == "true",
            values["skip"] == "true",
            json.loads(values["build-matrix"])["include"],
        )


def select_matrix(ref: str, event: str, requested: str) -> tuple[bool, list[dict[str, str]]]:
    publish, skip, lanes = plan(ref, event, requested)
    assert not skip
    return publish, lanes


class OsReleaseMatrixTests(unittest.TestCase):
    def test_version_tags_keep_the_dedicated_release_fleet(self):
        publish, lanes = select_matrix("refs/tags/v1.2.3", "push", "false")
        self.assertTrue(publish)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "imager-linux-x86_64"},
                {"arch": "aarch64", "runner": "imager-linux-aarch64"},
            ],
        )

    def test_protected_manual_main_publish_uses_native_large_disk_fleet(self):
        publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "true")
        self.assertTrue(publish)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "runner-large"},
                {"arch": "aarch64", "runner": "runner-large-arm64"},
            ],
        )

    def test_manual_nonpublication_builds_both_native_integration_lanes(self):
        publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "false")
        self.assertFalse(publish)
        self.assertEqual(lanes, [
            {"arch": "x86_64", "runner": "runner-large"},
            {"arch": "aarch64", "runner": "runner-large-arm64"},
        ])

    def test_nightly_main_publishes_through_the_manual_main_fleet(self):
        publish, skip, lanes = plan("refs/heads/main", "schedule", "", "f" * 40)
        self.assertTrue(publish)
        self.assertFalse(skip)
        self.assertEqual(
            lanes,
            [
                {"arch": "x86_64", "runner": "runner-large"},
                {"arch": "aarch64", "runner": "runner-large-arm64"},
            ],
        )

    def test_nightly_publishes_when_no_published_revision_is_readable(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", "")
        self.assertTrue(publish)
        self.assertFalse(skip)

    def test_nightly_skips_the_build_when_main_is_already_published(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", SHA)
        self.assertFalse(publish)
        self.assertTrue(skip)

    def test_schedule_never_publishes_from_another_ref(self):
        publish, skip, lanes = plan("refs/heads/feature", "schedule", "", "")
        self.assertFalse(publish)
        self.assertFalse(skip)
        self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-large"}])

    def test_unconfigured_nightly_runs_a_main_integration_build(self):
        publish, skip, lanes = plan("refs/heads/main", "schedule", "", "", "false")
        self.assertFalse(publish)
        self.assertFalse(skip)
        self.assertEqual(lanes, [
            {"arch": "x86_64", "runner": "runner-large"},
            {"arch": "aarch64", "runner": "runner-large-arm64"},
        ])

    def test_unconfigured_nightly_skips_an_already_built_integration_image(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", "", "false", SHA)
        self.assertFalse(publish)
        self.assertTrue(skip)

    def test_configured_nightly_publishes_despite_an_integration_image(self):
        publish, skip, _ = plan("refs/heads/main", "schedule", "", "", "true", SHA)
        self.assertTrue(publish)
        self.assertFalse(skip)

    def test_explicit_publication_ignores_the_published_revision(self):
        publish, skip, _ = plan("refs/heads/main", "workflow_dispatch", "true", SHA)
        self.assertTrue(publish)
        self.assertFalse(skip)

    def test_build_and_publication_jobs_honour_the_nightly_skip(self):
        workflow = WORKFLOW.read_text()
        build = workflow.split("\n  build-and-push:\n", 1)[1].split("\n    strategy:", 1)[0]
        self.assertIn("needs.plan.outputs.skip != 'true'", build)
        self.assertIn("PLANNED_PUBLISH: ${{ needs.plan.outputs.publish }}", workflow)
        self.assertIn("docker://code.podesta.ai/qos/tilo:latest", workflow)
        self.assertIn('.Labels["org.opencontainers.image.revision"]', workflow)

    def test_every_publishing_run_holds_the_generation_series_lock(self):
        cases = [
            ("refs/tags/v1.2.3", "push", ""),
            ("refs/heads/main", "workflow_dispatch", "true"),
            ("refs/heads/main", "workflow_dispatch", "false"),
            ("refs/heads/feature", "workflow_dispatch", "true"),
            ("refs/heads/main", "schedule", ""),
            ("refs/heads/feature", "schedule", ""),
            ("refs/heads/main", "push", ""),
        ]
        for ref, event, requested in cases:
            with self.subTest(ref=ref, event=event, requested=requested):
                # A scheduled main run that skips must still hold the lock, so
                # judge the lock against the unskipped rule.
                publish, _, _ = plan(ref, event, requested)
                group = concurrency_group(ref, event, requested)
                if publish:
                    self.assertEqual(group, "tilo-os-image-series")
                else:
                    self.assertEqual(group, f"tilo-os-image-validate-{ref}")


def generation_script() -> str:
    workflow = WORKFLOW.read_text()
    marker = "      - name: Resolve the image generation from the published series\n"
    # The step ends at the first blank line before the next step or its comment.
    block = re.split(r"\n\n      (?:#|- name:)", workflow.split(marker, 1)[1], maxsplit=1)[0]
    return textwrap.dedent(block.split("        run: |\n", 1)[1])


def empty_series_allowed(publish: str, genesis: str, sha: str = SHA) -> bool:
    """Run the generation step against a resolver that reports its bootstrap mode."""
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        resolver = root / "os/dev/next-generation.sh"
        resolver.parent.mkdir(parents=True)
        resolver.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "${TILO_ALLOW_EMPTY_SERIES:-0}"\n')
        resolver.chmod(0o755)
        environment = {
            key: value for key, value in os.environ.items() if key != "TILO_ALLOW_EMPTY_SERIES"
        }
        environment.update(
            {
                "GITHUB_ENV": str(root / "env"),
                "GITHUB_SHA": sha,
                "PUBLISH": publish,
                "SERIES_GENESIS_REVISION": genesis,
                "TILO_REGISTRY_CREDS": "user:token",
            }
        )
        subprocess.run(
            ["bash", "-c", generation_script()], cwd=root, env=environment,
            check=True, capture_output=True,
        )
        values = dict(line.split("=", 1) for line in (root / "env").read_text().splitlines())
        return values["TILO_GENERATION"] == "1"


class GenerationSeriesBootstrapTests(unittest.TestCase):
    def test_validation_builds_may_find_no_published_series(self):
        self.assertTrue(empty_series_allowed("false", ""))

    def test_publication_never_restarts_an_unreadable_series_by_default(self):
        self.assertFalse(empty_series_allowed("true", ""))

    def test_genesis_authorizes_only_the_exact_commit(self):
        self.assertTrue(empty_series_allowed("true", SHA))
        self.assertFalse(empty_series_allowed("true", "f" * 40))
        self.assertFalse(empty_series_allowed("true", SHA[:7], SHA[:7]))
        self.assertFalse(empty_series_allowed("true", SHA.upper(), SHA.upper()))

    def test_genesis_reads_the_protected_repository_variable(self):
        self.assertIn(
            "SERIES_GENESIS_REVISION: ${{ vars.TILO_OS_SERIES_GENESIS_REVISION }}",
            WORKFLOW.read_text(),
        )


class ReleaseGateTests(unittest.TestCase):
    def test_validation_gate_tolerates_an_absent_prediction_signer(self):
        # Only a publication exports the signer, and the gate runs under set -u.
        workflow = WORKFLOW.read_text()
        gate = workflow.split("      - name: Reseal, install, boot, and stage the verified image\n", 1)[1]
        gate = gate.split("\n      - name: ", 1)[0]
        self.assertIn("set -euo pipefail", gate)
        condition = next(line for line in gate.splitlines() if "sign-target-prediction.sh" in line)
        guard = gate.splitlines()[gate.splitlines().index(condition) - 1]
        self.assertIn('[[ -n "${TILO_PCR_PREDICTION_SIGNER:-}" ]]', guard)

    def test_oci_archive_reads_select_a_tagged_descriptor(self):
        workflow = WORKFLOW.read_text()
        archive = re.compile(r"oci-archive:/var/tmp/tilo-image(?:-repro)?\.tar")
        uses = list(archive.finditer(workflow))
        self.assertEqual(len(uses), 5)
        for use in uses:
            self.assertRegex(
                workflow[use.end():use.end() + 24],
                r"^:\$(?:source_tag|first_tag|repro_image)\b",
            )
        self.assertNotIn("podman load --input /var/tmp/tilo-image", workflow)

    @unittest.skipUnless(os.name == "posix" and shutil.which("skopeo"), "skopeo on POSIX required")
    def test_two_ref_archive_needs_an_explicit_source_reference(self):
        # Buildx emits one top-level OCI index descriptor for each image tag.
        # The selected ref must work for both inspection and copy/import.
        first = "code.podesta.ai/qos/tilo:gen1"
        second = "code.podesta.ai/qos/tilo:main-abcdef0"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            layout = root / "layout"
            layout.mkdir()

            def blob(payload: bytes) -> tuple[str, int]:
                digest = hashlib.sha256(payload).hexdigest()
                target = layout / "blobs" / "sha256" / digest
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(payload)
                return digest, len(payload)

            def encoded(value: dict) -> bytes:
                return json.dumps(value, separators=(",", ":")).encode()

            layer_buffer = io.BytesIO()
            with tarfile.open(fileobj=layer_buffer, mode="w"):
                pass
            layer_digest, layer_size = blob(layer_buffer.getvalue())
            config_digest, config_size = blob(encoded({
                "architecture": "arm64", "os": "linux", "config": {},
                "rootfs": {"type": "layers", "diff_ids": [f"sha256:{layer_digest}"]},
            }))
            manifest_digest, manifest_size = blob(encoded({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": {
                    "mediaType": "application/vnd.oci.image.config.v1+json",
                    "digest": f"sha256:{config_digest}", "size": config_size,
                },
                "layers": [{
                    "mediaType": "application/vnd.oci.image.layer.v1.tar",
                    "digest": f"sha256:{layer_digest}", "size": layer_size,
                }],
            }))
            other_config_digest, other_config_size = blob(encoded({
                "architecture": "arm64", "os": "linux",
                "config": {"Labels": {"variant": "other"}},
                "rootfs": {"type": "layers", "diff_ids": [f"sha256:{layer_digest}"]},
            }))
            other_manifest = json.loads(
                (layout / "blobs" / "sha256" / manifest_digest).read_text()
            )
            other_manifest["config"] = {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": f"sha256:{other_config_digest}", "size": other_config_size,
            }
            other_manifest_digest, other_manifest_size = blob(encoded(other_manifest))
            (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
            (layout / "index.json").write_bytes(encoded({
                "schemaVersion": 2,
                "manifests": [{
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": f"sha256:{digest}", "size": size,
                    "annotations": {"org.opencontainers.image.ref.name": ref},
                } for ref, digest, size in (
                    (first, manifest_digest, manifest_size),
                    (second, other_manifest_digest, other_manifest_size),
                )],
            }))
            archive = root / "image.tar"
            with tarfile.open(archive, "w") as tar:
                for path in sorted(layout.rglob("*"), key=lambda p: (len(p.parts), p.as_posix())):
                    if path.is_dir():
                        entry = tarfile.TarInfo(path.relative_to(layout).as_posix() + "/")
                        entry.type = tarfile.DIRTYPE
                        entry.mode = 0o755
                    else:
                        payload = path.read_bytes()
                        entry = tarfile.TarInfo(path.relative_to(layout).as_posix())
                        entry.size = len(payload)
                        entry.mode = 0o644
                    entry.uid = os.getuid()
                    entry.gid = os.getgid()
                    if path.is_dir():
                        tar.addfile(entry)
                    else:
                        tar.addfile(entry, io.BytesIO(payload))

            ambiguous = subprocess.run(
                ["skopeo", "inspect", "--raw", f"oci-archive:{archive}"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(ambiguous.returncode, 0)
            chosen = subprocess.run(
                ["skopeo", "inspect", "--raw", f"oci-archive:{archive}:{first}"],
                capture_output=True, text=True,
            )
            self.assertEqual(chosen.returncode, 0, chosen.stderr)
            self.assertEqual(hashlib.sha256(chosen.stdout.encode()).hexdigest(), manifest_digest)
            other = subprocess.run(
                ["skopeo", "inspect", "--raw", f"oci-archive:{archive}:{second}"],
                capture_output=True, text=True, check=True,
            )
            self.assertEqual(hashlib.sha256(other.stdout.encode()).hexdigest(), other_manifest_digest)
            subprocess.run(
                ["skopeo", "copy", f"oci-archive:{archive}:{first}",
                 f"dir:{root / 'selected'}"],
                capture_output=True, text=True, check=True,
            )


def concurrency_group(ref: str, event: str, requested: str) -> str:
    """Evaluate the workflow's concurrency expression for one event."""
    line = next(
        line for line in WORKFLOW.read_text().splitlines() if line.startswith("  group: ${{")
    )
    expression = line.split("${{", 1)[1].rsplit("}}", 1)[0]
    for source, target in [
        ("startsWith(github.ref, 'refs/tags/v')", "ref.startswith('refs/tags/v')"),
        ("format('tilo-os-image-validate-{0}', github.ref)", "('tilo-os-image-validate-' + ref)"),
        ("github.event.inputs.publish", "requested"),
        ("github.event_name", "event"),
        ("github.ref", "ref"),
        ("&&", " and "),
        ("||", " or "),
    ]:
        expression = expression.replace(source, target)
    return eval(expression, {}, {"ref": ref, "event": event, "requested": requested})


if __name__ == "__main__":
    unittest.main()
