"""Contract for the dev VM disk workflow and its request from the OS release."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/dev-disk.yml"
OS_RELEASE = ROOT / ".forgejo/workflows/build-image.yml"


def job(source: str, name: str) -> str:
    """Return the text of one top-level job."""
    body = source.split(f"\n  {name}:\n", 1)[1]
    following = re.search(r"\n  [A-Za-z0-9_-]+:\n", body)
    return body[: following.start()] if following else body


def step(source: str, name: str) -> str:
    """Return the text of one named step."""
    body = source.split(f"      - name: {name}\n", 1)[1]
    following = re.search(r"\n      - (?:name|uses):", body)
    return body[: following.start()] if following else body


class DevDiskWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = WORKFLOW.read_text(encoding="utf-8")

    def test_runs_only_on_explicit_dispatch(self) -> None:
        triggers = self.source.split("\non:\n", 1)[1].split("\nconcurrency:", 1)[0]
        self.assertIn("  workflow_dispatch:\n", triggers)
        for event in ("push:", "pull_request:", "schedule:"):
            self.assertNotIn(event, triggers)
        self.assertIn("group: tilo-vm-disk\n  cancel-in-progress: false", self.source)

    def test_native_lanes_and_formats_are_one_entry_away(self) -> None:
        plan = job(self.source, "plan")
        self.assertIn(
            "declare -A runners=([x86_64]=runner-disk-100g [aarch64]=runner-disk-100g-arm64)",
            plan,
        )
        self.assertIn("qcow2|vhdx) ;;", plan)
        self.assertIn('default: "x86_64"', self.source)
        self.assertIn('default: "qcow2"', self.source)
        build = job(self.source, "build")
        self.assertIn("matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}", build)
        self.assertIn('./os/dev/assert-native-arch.sh "$ARCH"', build)

    def test_dev_image_builds_on_the_verified_same_source_base(self) -> None:
        build = job(self.source, "build")
        self.assertIn("ref: ${{ needs.plan.outputs.source }}", build)
        verify = step(build, "Resolve and verify the signed production base")
        self.assertIn('cosign verify --key keys/cosign.pub "$top"', verify)
        self.assertIn('cosign verify --key keys/cosign.pub "$child"', verify)
        self.assertIn('"git+ssh://code.sandbox.example/qos/qos@" + $source', verify)
        image = step(build, "Build the dev image FROM the verified base")
        self.assertIn("--file os/Containerfile.dev", image)
        self.assertIn('--build-arg "BASE_IMAGE=$BASE_CHILD"', image)
        self.assertIn("BASE_CHILD: ${{ steps.base.outputs.child }}", image)
        self.assertLess(build.index("Resolve and verify"), build.index("Build the dev image"))

    def test_disk_is_sealed_booted_then_published(self) -> None:
        build = job(self.source, "build")
        disk = step(build, "Install the dev image to a bootable disk")
        self.assertIn('TILO_SKIP_BUILD: "1"', disk)
        self.assertIn("./os/dev/build-image.sh", disk)
        self.assertIn(
            "TILO_TARGET_IMGREF: code.sandbox.example/qos/tilo-vm:${{ needs.plan.outputs.tag }}",
            disk,
        )
        smoke = step(build, "Boot the disk and reach it over SSH")
        self.assertIn('qemu-img create -f qcow2 -F qcow2 -b "$TILO_DISK"', smoke)
        self.assertIn("./os/dev/vm.sh ssh true", smoke)
        order = [
            "Install the dev image to a bootable disk",
            "Boot the disk and reach it over SSH",
            "Push the dev image child",
            "Convert, hash and describe the disks",
            "Publish the immutable disks",
        ]
        positions = [build.index(f"- name: {name}") for name in order]
        self.assertEqual(positions, sorted(positions))

    def test_disks_use_immutable_generic_packages(self) -> None:
        build = job(self.source, "build")
        convert = step(build, "Convert, hash and describe the disks")
        self.assertIn("-O qcow2 -c -o compression_type=zstd", convert)
        self.assertIn("-O vhdx -o subformat=dynamic", convert)
        publish = step(build, "Publish the immutable disks")
        self.assertIn("vars.TILO_VM_DISK_RELEASE_AUDIENCE", publish)
        self.assertIn('"$package_api/tilo-vm-disk/$DEV_TAG/$file"', publish)
        self.assertIn("tools/upload_immutable_packages.sh", publish)
        self.assertNotIn("REGISTRY_TOKEN", publish)
        self.assertNotIn("actions/cache", self.source)

    def test_manifest_commits_after_the_durable_tag_and_latest_moves_last(self) -> None:
        publish = job(self.source, "publish")
        order = [
            "Assemble the tilo-vm index and disk manifest",
            "Publish the disk manifest",
            "Advance tilo-vm:latest",
        ]
        positions = [publish.index(f"- name: {name}") for name in order]
        self.assertEqual(positions, sorted(positions))
        assemble = step(publish, "Assemble the tilo-vm index and disk manifest")
        self.assertIn("already binds", assemble)
        for field in ("tag:", "baseDigest:", "arch:", "format:", "sha256:", "size:"):
            self.assertIn(field, assemble)
        latest = step(publish, "Advance tilo-vm:latest")
        self.assertIn("if: needs.plan.outputs.channel == 'true'", latest)
        self.assertIn("git merge-base --is-ancestor", latest)

    def test_dev_images_are_never_release_signed(self) -> None:
        self.assertNotIn("COSIGN_KEY_REF", self.source)
        self.assertNotRegex(self.source, r"cosign (?:sign|attest)")


class OsReleaseRequestsDevDisksTests(unittest.TestCase):
    def test_published_release_requests_disks_for_its_exact_digest(self) -> None:
        request = job(OS_RELEASE.read_text(encoding="utf-8"), "dispatch-dev-disk")
        self.assertIn("needs: [plan, promote-multiarch]", request)
        self.assertIn("if: needs.plan.outputs.publish == 'true'", request)
        self.assertIn("name: tilo-os-release-index", request)
        self.assertIn("actions/workflows/dev-disk.yml/dispatches", request)
        self.assertIn("source_revision:$source,base_image:$base,tag:$tag", request)


if __name__ == "__main__":
    unittest.main()
