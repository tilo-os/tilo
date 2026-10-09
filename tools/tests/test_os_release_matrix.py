from pathlib import Path
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/build-image.yml"


def matrix_script() -> str:
    workflow = WORKFLOW.read_text()
    marker = "      - name: Select validation or native publication lanes\n"
    block = workflow.split(marker, 1)[1].split("\n      - name:", 1)[0].split("\n\n  signing-preflight:", 1)[0]
    return textwrap.dedent(block.split("        run: |\n", 1)[1])


SHA = "0123456789abcdef0123456789abcdef01234567"


def plan(
    ref: str,
    event: str,
    requested: str,
    published: str = "",
    configured: str = "true",
    integration: str = "",
    arches: str = "",
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
                "REQUESTED_RELEASE": requested,
                "PUBLISHED_REVISION": published,
                "PUBLICATION_CONFIGURED": configured,
                "INTEGRATION_REVISION": integration,
                "DEV_ARCHES": arches,
            }
        )
        subprocess.run(["bash", "-c", matrix_script()], env=environment, check=True)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        assert values["mode"] == ("release" if values["publish"] == "true" else "dev")
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
    def test_release_ref_and_event_choose_the_native_build_fleet(self):
        with self.subTest(phase='version tags keep the dedicated release fleet'):
            publish, lanes = select_matrix("refs/tags/v1.2.3", "push", "false")
            self.assertTrue(publish)
            self.assertEqual(
                lanes,
                [
                    {"arch": "x86_64", "runner": "imager-linux-x86_64"},
                    {"arch": "aarch64", "runner": "imager-linux-aarch64"},
                ],
            )
        with self.subTest(phase='protected manual main publish uses the release class'):
            publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "true")
            self.assertTrue(publish)
            self.assertEqual(
                lanes,
                [
                    {"arch": "x86_64", "runner": "runner-release"},
                    {"arch": "aarch64", "runner": "runner-release-arm64"},
                ],
            )
        with self.subTest(phase='manual nonpublication builds both native integration lanes'):
            publish, lanes = select_matrix("refs/heads/main", "workflow_dispatch", "false")
            self.assertFalse(publish)
            self.assertEqual(lanes, [
                {"arch": "x86_64", "runner": "runner-large"},
                {"arch": "aarch64", "runner": "runner-large-arm64"},
            ])
        with self.subTest(phase='nightly main publishes through the manual main fleet'):
            publish, skip, lanes = plan("refs/heads/main", "schedule", "", "f" * 40)
            self.assertTrue(publish)
            self.assertFalse(skip)
            self.assertEqual(
                lanes,
                [
                    {"arch": "x86_64", "runner": "runner-release"},
                    {"arch": "aarch64", "runner": "runner-release-arm64"},
                ],
            )

    def test_nightly_builds_publish_or_skip_based_on_published_and_built_revisions(self):
        with self.subTest(phase='nightly publishes when no published revision is readable'):
            publish, skip, _ = plan("refs/heads/main", "schedule", "", "")
            self.assertTrue(publish)
            self.assertFalse(skip)
        with self.subTest(phase='nightly skips the build when main is already published'):
            publish, skip, _ = plan("refs/heads/main", "schedule", "", SHA)
            self.assertFalse(publish)
            self.assertTrue(skip)
        with self.subTest(phase='schedule never publishes from another ref'):
            publish, skip, lanes = plan("refs/heads/feature", "schedule", "", "")
            self.assertFalse(publish)
            self.assertFalse(skip)
            self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-large"}])
        with self.subTest(phase='unconfigured nightly runs a main integration build'):
            publish, skip, lanes = plan("refs/heads/main", "schedule", "", "", "false")
            self.assertFalse(publish)
            self.assertFalse(skip)
            self.assertEqual(lanes, [
                {"arch": "x86_64", "runner": "runner-large"},
                {"arch": "aarch64", "runner": "runner-large-arm64"},
            ])
        with self.subTest(phase='unconfigured nightly skips an already built integration image'):
            publish, skip, _ = plan("refs/heads/main", "schedule", "", "", "false", SHA)
            self.assertFalse(publish)
            self.assertTrue(skip)
        with self.subTest(phase='configured nightly publishes despite an integration image'):
            publish, skip, _ = plan("refs/heads/main", "schedule", "", "", "true", SHA)
            self.assertTrue(publish)
            self.assertFalse(skip)
        with self.subTest(phase='explicit publication ignores the published revision'):
            publish, skip, _ = plan("refs/heads/main", "workflow_dispatch", "true", SHA)
            self.assertTrue(publish)
            self.assertFalse(skip)

    def test_publishing_jobs_honour_the_nightly_skip_and_hold_the_series_lock(self):
        with self.subTest(phase='build and publication jobs honour the nightly skip'):
            workflow = WORKFLOW.read_text()
            build = workflow.split("\n  build-and-push:\n", 1)[1].split("\n    strategy:", 1)[0]
            self.assertIn("needs.plan.outputs.skip != 'true'", build)
            self.assertIn("PLANNED_PUBLISH: ${{ needs.plan.outputs.publish }}", workflow)
            self.assertIn("docker://registry.tilo.so/tilo/tilo:latest", workflow)
            self.assertIn('.Labels["org.opencontainers.image.revision"]', workflow)
        with self.subTest(phase='generic publishing runs hold the generation series lock'):
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
                        self.assertEqual(group, "tilo-os-image-series-2")
                    else:
                        self.assertEqual(group, "tilo-os-image-validate-2")
        with self.subTest(phase='full branch proofs share a queue without cancelling another owner'):
            branches = ("refs/heads/feature-a", "refs/heads/feature-b")
            groups = [concurrency_group(ref, "workflow_dispatch", "false") for ref in branches]
            self.assertEqual(groups, ["tilo-os-image-validate-2"] * 2)
            self.assertNotEqual(groups[0], concurrency_group(branches[0], "workflow_call", ""))
            self.assertIn("cancel-in-progress: ${{ github.event_name == 'workflow_call' && github.ref != 'refs/heads/main' }}", WORKFLOW.read_text())
        with self.subTest(phase='Lenovo publications serialize the board generation series'):
            self.assertEqual(
                concurrency_group("refs/heads/main", "workflow_dispatch", "true", "lenovo-83ed"),
                "tilo-os-image-lenovo-series",
            )
            self.assertEqual(
                concurrency_group("refs/heads/main", "workflow_dispatch", "true", "lenovo-83ed", "f" * 40),
                "tilo-os-image-lenovo-series",
            )
            self.assertNotEqual(
                concurrency_group("refs/heads/main", "workflow_dispatch", "true"),
                concurrency_group("refs/heads/main", "workflow_dispatch", "true", "lenovo-83ed"),
            )

        with self.subTest(phase='Orange Pi publication has a separate source-specific lock'):
            orange = concurrency_group("refs/heads/main", "workflow_dispatch", "true", "orangepi-3b-v2-1")
            self.assertEqual(orange, f"tilo-os-image-orangepi-{SHA}")
            self.assertNotEqual(orange, concurrency_group("refs/heads/main", "workflow_dispatch", "true"))
            self.assertNotEqual(orange, concurrency_group("refs/heads/main", "workflow_dispatch", "true", "lenovo-83ed"))


class OsDevLaneTests(unittest.TestCase):
    def test_build_os_dev_calls_a_cached_image_only_dev_build(self):
        with self.subTest(phase='a called build is a dev build of both native lanes'):
            for ref in ("refs/heads/main", "refs/pull/12/head"):
                publish, skip, lanes = plan(ref, "workflow_call", "")
                self.assertFalse(publish)
                self.assertFalse(skip)
                self.assertEqual(lanes, [
                    {"arch": "x86_64", "runner": "runner-large"},
                    {"arch": "aarch64", "runner": "runner-large-arm64"},
                ])
        with self.subTest(phase='a pull request builds the architectures os-dev chose'):
            _, _, lanes = plan("refs/pull/12/head", "workflow_call", "", arches='["x86_64"]')
            self.assertEqual(lanes, [{"arch": "x86_64", "runner": "runner-large"}])
            self.assertIn("arches: ${{ needs.arches.outputs.arches }}",
                          (ROOT / ".forgejo/workflows/os-dev.yml").read_text())
        with self.subTest(phase='a newer commit of the ref supersedes the build'):
            self.assertEqual(
                concurrency_group("refs/pull/12/head", "workflow_call", ""),
                "tilo-os-dev-refs/pull/12/head",
            )
            self.assertIn("cancel-in-progress: ${{ github.event_name == 'workflow_call' && github.ref != 'refs/heads/main' }}", WORKFLOW.read_text())
        with self.subTest(phase='each main commit builds on its own'):
            self.assertEqual(
                concurrency_group("refs/heads/main", "workflow_call", ""),
                "tilo-os-dev-" + SHA,
            )
        with self.subTest(phase='main pushes and image pull requests call it'):
            caller = (ROOT / ".forgejo/workflows/os-dev.yml").read_text()
            self.assertIn("name: Build / OS dev", caller)
            self.assertIn("uses: ./.forgejo/workflows/build-image.yml", caller)
            self.assertIn("branches: [main]", caller)
            self.assertIn('- "os/**"', caller)
            self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}", caller)
            self.assertIn("  workflow_call:", WORKFLOW.read_text())
        with self.subTest(phase='the dev build publishes images and skips the disk install'):
            workflow = WORKFLOW.read_text()
            step = workflow.split("      - name: Build and publish the dev image\n", 1)[1].split("\n      - name: ", 1)[0]
            self.assertIn("steps.release-mode.outputs.dev-fast == 'true'", step)
            self.assertIn("-f os/Containerfile.dev", step)
            self.assertNotIn("--cache-to", step)
            self.assertIn("--secret id=actions_cache_url,env=ACTIONS_CACHE_URL", step)
            self.assertIn("push=true", step)
            self.assertIn("PUSHED_DIGEST=", step)
            self.assertIn("IMAGE_ONLY=1", step)
            self.assertIn("TILO_DEV_RUST_LTO=off", step)
            gate = workflow.split("      - name: Reseal, install, boot, and stage the verified image\n", 1)[1]
            self.assertIn("steps.release-mode.outputs.dev-fast != 'true'", gate.splitlines()[1])

    def test_dev_images_carry_the_apps_of_their_commit(self):
        workflow = WORKFLOW.read_text()
        step = workflow.split("      - name: Materialize the pinned app collection\n", 1)[1]
        step = step.split("\n      - name:", 1)[0]
        self.assertNotIn("        if:", step)
        # A dev build takes the published collection only when its source has
        # this commit's app inputs, as Release / apps defines them.
        self.assertIn('git diff --quiet "$COLLECTION_SOURCE" "$GITHUB_SHA" -- "${inputs[@]}"', step)
        self.assertIn(".forgejo/workflows/release-apps.yml", step)
        self.assertIn("index.json | jq -r '.source // empty'", step)
        self.assertLess(step.index("[[ $source != \"$COLLECTION_SOURCE\" ]]"),
                        step.index("echo 'TILO_PREBUILT_APPS=1'"))
        self.assertLess(workflow.index("echo 'TILO_PREBUILT_APPS=0' >> \"$GITHUB_ENV\""),
                        workflow.index("      - name: Materialize the pinned app collection\n"))

    def test_dev_builds_keep_a_cacheable_epoch(self):
        workflow = WORKFLOW.read_text()
        step = workflow.split("      - name: Compute SOURCE_DATE_EPOCH (HEAD commit time)\n", 1)[1]
        step = step.split("\n      - name:", 1)[0]
        self.assertIn("DEV_BUILD: ${{ github.event_name == 'workflow_call' }}", step)
        self.assertIn('for ref in "$BASE_REF" "$RPMS_REF"; do', step)
        # The kernel snapshot changes several times a day; in the epoch it
        # invalidated every rootfs layer.
        self.assertNotIn("KERNEL_RPMS_REF", step.split("if [[ $DEV_BUILD == true ]]", 1)[1])
        # image.created keeps the commit time either way.
        self.assertLess(step.index("SOURCE_DATE_ISO="), step.index("if [[ $DEV_BUILD == true ]]"))

    def test_dev_series_names_the_ref_and_commit(self):
        workflow = WORKFLOW.read_text()
        block = workflow.split("      - name: Select build, validation, or publication mode\n", 1)[1]
        script = textwrap.dedent(block.split("        run: |\n", 1)[1].split("\n\n      ", 1)[0])
        for ref, event, series, fast, gate in [
            ("refs/heads/main", "workflow_call", "main-0123456", "true", "false"),
            ("refs/pull/12/head", "workflow_call", "pr-12-0123456", "true", "false"),
            ("refs/heads/fix/x", "workflow_dispatch", "fix-x-0123456", "false", "true"),
        ]:
            with self.subTest(ref=ref, event=event), tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "output"
                env = os.environ.copy()
                env.update({
                    "GITHUB_REF": ref, "GITHUB_REF_NAME": ref.split("/", 2)[2],
                    "GITHUB_EVENT_NAME": event, "GITHUB_SHA": SHA,
                    "GITHUB_OUTPUT": str(output), "GITHUB_ENV": str(Path(temporary) / "env"),
                    "REQUESTED_RELEASE": "", "PLANNED_PUBLISH": "false",
                })
                subprocess.run(["bash", "-c", script.replace("${{ matrix.role }}", "")],
                               env=env, check=True, stdout=subprocess.DEVNULL)
                values = dict(line.split("=", 1) for line in output.read_text().splitlines())
                self.assertEqual(values["dev-series"], series)
                self.assertEqual(values["dev-fast"], fast)
                self.assertEqual(values["dev-gate"], gate)


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
    def test_empty_generation_series_is_allowed_only_for_validation_or_exact_genesis(self):
        with self.subTest(phase='validation builds may find no published series'):
            self.assertTrue(empty_series_allowed("false", ""))
        with self.subTest(phase='publication never restarts an unreadable series by default'):
            self.assertFalse(empty_series_allowed("true", ""))
        with self.subTest(phase='genesis authorizes only the exact commit'):
            self.assertTrue(empty_series_allowed("true", SHA))
            self.assertFalse(empty_series_allowed("true", "f" * 40))
            self.assertFalse(empty_series_allowed("true", SHA[:7], SHA[:7]))
            self.assertFalse(empty_series_allowed("true", SHA.upper(), SHA.upper()))
        with self.subTest(phase='genesis reads the protected repository variable'):
            self.assertIn(
                "SERIES_GENESIS_REVISION: ${{ vars.TILO_OS_SERIES_GENESIS_REVISION }}",
                WORKFLOW.read_text(),
            )


class ReleaseGateTests(unittest.TestCase):
    def test_validation_gate_and_oci_archive_reads_are_exact(self):
        with self.subTest(phase='validation gate tolerates an absent prediction signer'):
            # Only a publication exports the signer, and the gate runs under set -u.
            workflow = WORKFLOW.read_text()
            gate = workflow.split("      - name: Reseal, install, boot, and stage the verified image\n", 1)[1]
            gate = gate.split("\n      - name: ", 1)[0]
            self.assertIn("set -euo pipefail", gate)
            condition = next(line for line in gate.splitlines() if "sign-target-prediction.sh" in line)
            guard = gate.splitlines()[gate.splitlines().index(condition) - 1]
            self.assertIn('[[ -n "${TILO_PCR_PREDICTION_SIGNER:-}" ]]', guard)
        with self.subTest(phase='oci archive reads select a tagged descriptor'):
            workflow = WORKFLOW.read_text()
            archive = re.compile(r"oci-archive:/var/tmp/tilo-image(?:-repro)?\.tar")
            uses = list(archive.finditer(workflow))
            self.assertEqual(len(uses), 6)
            for use in uses:
                self.assertRegex(
                    workflow[use.end():use.end() + 24],
                    r"^:\$(?:source_ref|first_ref|repro_ref)\b",
                )
            self.assertIn("'${{ steps.build.outputs.digest || steps.build-uncached.outputs.digest }}'", workflow)
            self.assertIn("'${{ steps.repro.outputs.digest }}'", workflow)
            self.assertNotIn("podman load --input /var/tmp/tilo-image", workflow)

    @unittest.skipUnless(os.name == "posix" and shutil.which("skopeo"), "skopeo on POSIX required")
    def test_two_ref_archive_needs_an_explicit_source_reference(self):
        # Buildx emits one top-level OCI index descriptor for each image tag.
        # The selected ref must work for both inspection and copy/import.
        first = "registry.tilo.so/tilo/tilo:gen1"
        second = "registry.tilo.so/tilo/tilo:main-abcdef0"
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
            (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
            (layout / "index.json").write_bytes(encoded({
                "schemaVersion": 2,
                "manifests": [{
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": f"sha256:{manifest_digest}", "size": manifest_size,
                    "annotations": {
                        "io.containerd.image.name": ref,
                        "org.opencontainers.image.ref.name": ref.rsplit(":", 1)[1],
                    },
                } for ref in (first, second)],
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
            selector = ROOT / "os/scripts/oci-archive-ref.py"
            chosen_ref = subprocess.check_output(
                [sys.executable, str(selector), str(archive), first,
                 f"sha256:{manifest_digest}"], text=True,
            ).strip()
            self.assertEqual(chosen_ref, "gen1")
            wrong_digest = subprocess.run(
                [sys.executable, str(selector), str(archive), first,
                 f"sha256:{'0' * 64}"], capture_output=True, text=True,
            )
            self.assertNotEqual(wrong_digest.returncode, 0)
            chosen = subprocess.run(
                ["skopeo", "inspect", "--raw", f"oci-archive:{archive}:{chosen_ref}"],
                capture_output=True, text=True,
            )
            self.assertEqual(chosen.returncode, 0, chosen.stderr)
            self.assertEqual(hashlib.sha256(chosen.stdout.encode()).hexdigest(), manifest_digest)
            subprocess.run(
                ["skopeo", "copy", f"oci-archive:{archive}:{chosen_ref}",
                 f"dir:{root / 'selected'}"],
                capture_output=True, text=True, check=True,
            )


def concurrency_group(
    ref: str, event: str, requested: str, profile: str = "generic", sha: str = SHA,
) -> str:
    """Evaluate the workflow's concurrency expression for one event."""
    line = next(
        line for line in WORKFLOW.read_text().splitlines() if line.startswith("  group: ${{")
    )
    expression = line.split("${{", 1)[1].rsplit("}}", 1)[0]
    for source, target in [
        ("startsWith(github.ref, 'refs/tags/v')", "ref.startswith('refs/tags/v')"),
        ("format('tilo-os-image-lenovo-{0}', github.sha)", "('tilo-os-image-lenovo-' + sha)"),
        ("format('tilo-os-image-orangepi-{0}', github.sha)", "('tilo-os-image-orangepi-' + sha)"),
        ("format('tilo-os-image-validate-{0}', github.ref)", "('tilo-os-image-validate-' + ref)"),
        ("format('tilo-os-dev-{0}', github.ref == 'refs/heads/main' && github.sha || github.ref)",
         "('tilo-os-dev-' + (sha if ref == 'refs/heads/main' else ref))"),
        ("github.event.inputs.hardware_profile", "profile"),
        ("github.event.inputs.release", "requested"),
        ("github.event_name", "event"),
        ("github.ref", "ref"),
        ("&&", " and "),
        ("||", " or "),
    ]:
        expression = expression.replace(source, target)
    return eval(expression, {}, {
        "ref": ref, "event": event, "requested": requested, "profile": profile, "sha": sha,
    })


if __name__ == "__main__":
    unittest.main()
