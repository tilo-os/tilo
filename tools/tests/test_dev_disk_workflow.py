"""Contract for publishing dev VM disks: per architecture, from the lane that booted them."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/dev-disk.yml"
OS_RELEASE = ROOT / ".forgejo/workflows/build-image.yml"
PUBLISH = ROOT / "os/dev/publish-dev-disk.sh"
CANDIDATE = ROOT / "os/dev/publish-dev-disk-candidate.sh"
CHECK = ROOT / "os/dev/check-dev-vm.sh"
STORE = ROOT / "os/dev/ci-native-podman-store.sh"


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

    def test_dev_disks_build_on_explicit_dispatch_from_the_verified_native_base(self) -> None:
        with self.subTest(phase='runs only on explicit dispatch'):
            triggers = self.source.split("\non:\n", 1)[1].split("\nconcurrency:", 1)[0]
            self.assertIn("  workflow_dispatch:\n", triggers)
            for event in ("push:", "pull_request:", "schedule:"):
                self.assertNotIn(event, triggers)
            # Per-architecture requests must not replace each other while queued.
            self.assertIn("group: tilo-vm-disk-${{ github.event.inputs.tag }}-", self.source)
            self.assertIn("${{ github.event.inputs.architectures }}", self.source)
        with self.subTest(phase='native lanes and formats are one entry away'):
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
            self.assertIn("fail-fast: false", build)
            self.assertIn('./os/dev/assert-native-arch.sh "$ARCH"', build)
        with self.subTest(phase='dev disk build uses measured native overlay'):
            build = job(self.source, "build")
            devices = step(build, "Expose guest FUSE and loop devices")
            native = step(build, "Use native Podman overlay for the dev disk")
            login = step(build, "Log in to registry.tilo.so")
            self.assertIn("sudo -n losetup --find >/dev/null", devices)
            self.assertIn("./os/dev/ci-native-podman-store.sh", native)
            self.assertNotIn("if:", native)
            store = STORE.read_text(encoding="utf-8")
            self.assertIn('sudo -n mount -t ext4 "$loop" "$root"', store)
            self.assertIn('export CONTAINERS_STORAGE_CONF="$root/storage.conf"', store)
            self.assertIn('.store.graphStatus["Backing Filesystem"] == "extfs"', store)
            self.assertIn('.has-mount-program', store)
            self.assertIn('>> "$GITHUB_ENV"', store)
            self.assertLess(build.index("Expose guest FUSE"), build.index("Use native Podman overlay"))
            self.assertLess(build.index("Use native Podman overlay"), build.index("Log in to registry.tilo.so"))
            self.assertIn("registry: registry.tilo.so", login)
            self.assertIn("password: ${{ secrets.GITHUB_TOKEN }}", login)
        with self.subTest(phase='architectures never wait for each other'):
            # No job joins the architectures: each lane publishes itself.
            self.assertNotIn("\n  publish:\n", self.source)
            self.assertNotIn("download-artifact", self.source)
            build = job(self.source, "build")
            publish = step(build, "Publish this architecture's dev image and disk")
            self.assertIn("./os/dev/publish-dev-disk.sh", publish)
            self.assertIn("CHANNEL: ${{ needs.plan.outputs.channel }}", publish)
        with self.subTest(phase='dev image builds on the verified same source base'):
            build = job(self.source, "build")
            self.assertIn("ref: ${{ needs.plan.outputs.source }}", build)
            verify = step(build, "Resolve and verify the same-commit base")
            self.assertIn('cosign verify --key keys/cosign.pub "$top"', verify)
            self.assertIn('cosign verify --key keys/cosign.pub "$child"', verify)
            self.assertIn('"git+ssh://code.tilo.so/tilo/tilo@" + $source', verify)
            # A dev-trust base skips signatures only because its repository says
            # so, and must still name the exact source revision.
            self.assertIn("release) repository=registry.tilo.so/tilo/tilo ;;", verify)
            self.assertIn("integration) repository=registry.tilo.so/tilo/tilo-ci ;;", verify)
            self.assertIn('.Labels["org.opencontainers.image.revision"] // empty', verify)
            self.assertIn('== "$SOURCE_REVISION"', verify)
            image = step(build, "Build the dev image FROM the verified base")
            # The recipe developers run, FROM the verified child digest.
            self.assertIn("./os/dev/build-dev-image.sh", image)
            self.assertIn('podman pull "$BASE_CHILD"', image)
            self.assertIn('TILO_DEV_BASE="$BASE_CHILD"', image)
            self.assertIn("BASE_CHILD: ${{ steps.base.outputs.child }}", image)
            self.assertNotIn("--file os/Containerfile.dev", self.source)
            self.assertLess(build.index("Resolve and verify"), build.index("Build the dev image"))
        with self.subTest(phase='older os source uses explicitly recorded disk tooling'):
            build = job(self.source, "build")
            image = step(build, "Build the dev image FROM the verified base")
            checkout = step(build, "Checkout exact disk tooling revision")
            apply = step(build, "Apply reviewed disk tooling only")
            disk = step(build, "Install the dev image to a bootable disk")
            self.assertIn("SOURCE_REVISION: ${{ needs.plan.outputs.source }}", build)
            self.assertIn("DISK_TOOLING_REVISION: ${{ github.sha }}", build)
            self.assertIn("ref: ${{ github.sha }}", checkout)
            self.assertIn("sparse-checkout-cone-mode: false", checkout)
            for file in (
                "disk-image.sh",
                "check-composefs-verity.py",
                "export-direct-boot-bundle.sh",
                "hold-loop-partition-nodes.py",
                "private_disk_image.py",
                "check-dev-vm.sh",
                "publish-dev-disk.sh",
                "_common.sh",
            ):
                self.assertIn(file, checkout)
                self.assertIn(file, apply)
            self.assertIn("git -C .ci-disk-tooling rev-parse HEAD", apply)
            self.assertNotIn("cmp .ci-disk-tooling/os/dev/_common.sh os/dev/_common.sh", apply)
            self.assertLess(build.index(image), build.index(checkout))
            self.assertLess(build.index(apply), build.index(disk))
        with self.subTest(phase='preinstall checkpoint is digest pinned and verified on resume'):
            self.assertIn("dev_image:", self.source)
            plan = job(self.source, "plan")
            self.assertIn("checkpoint=${INPUT_CHECKPOINT:-}", plan)
            self.assertIn("ne 1", plan)
            build = job(self.source, "build")
            image = step(build, "Build the dev image FROM the verified base")
            push = step(build, "Push preinstall dev-image checkpoint")
            resume = step(build, "Load and verify preinstall dev-image checkpoint")
            self.assertIn("if: needs.plan.outputs.checkpoint == ''", image)
            self.assertIn("org.opencontainers.image.base.child.digest=$BASE_CHILD_DIGEST", image)
            self.assertIn("podman push --digestfile", push)
            self.assertIn('podman pull "$DEV_CHECKPOINT_REF"', resume)
            self.assertIn('if: needs.plan.outputs.checkpoint != \'\'', resume)
            for label in (
                "sh.tilo.image.flavour",
                "org.opencontainers.image.revision",
                "org.opencontainers.image.base.name",
                "org.opencontainers.image.base.digest",
                "org.opencontainers.image.base.child.digest",
                "sh.tilo.dev.base-trust",
            ):
                self.assertIn(label, resume)
            self.assertLess(build.index(image), build.index(push))
            self.assertLess(build.index(resume), build.index("Install the dev image to a bootable disk"))

    def test_integration_candidate_succeeds_without_slow_tcg_boot(self) -> None:
        plan = job(self.source, "plan")
        build = job(self.source, "build")
        smoke = step(build, "Boot the disk and reach tilo-vmd over SSH")
        candidate = step(build, "Publish unvalidated installed disk candidate")
        validated = step(build, "Publish this architecture's dev image and disk")
        self.assertIn("validate_boot:", self.source)
        self.assertIn("auto) [[ $trust == release ]] && validate_boot=true || validate_boot=false", plan)
        self.assertIn('echo "validate_boot=$validate_boot"', plan)
        self.assertIn("if: github.event.inputs.validate_greeter != 'true'", candidate)
        self.assertIn("if: needs.plan.outputs.validate_boot == 'true'", smoke)
        self.assertIn('python3 os/dev/probe-kvm.py "$ARCH"', smoke)
        self.assertIn("export TILO_BOOT_WAIT_ATTEMPTS=120", smoke)
        self.assertIn("pgrep -x tilo-greeter", smoke)
        self.assertIn(
            "if: needs.plan.outputs.validate_boot == 'true' && github.event.inputs.validate_greeter != 'true'",
            validated,
        )

    def test_dev_disks_are_installed_booted_and_published_without_release_signing(self) -> None:
        with self.subTest(phase='disk is installed booted then published'):
            build = job(self.source, "build")
            disk = step(build, "Install the dev image to a bootable disk")
            self.assertIn('TILO_SKIP_BUILD: "1"', disk)
            self.assertIn('TILO_QCOW2_COMPRESS: "1"', disk)
            self.assertIn("./os/dev/build-image.sh", disk)
            self.assertIn(
                "TILO_TARGET_IMGREF: registry.tilo.so/tilo/tilo-vm:${{ needs.plan.outputs.tag }}",
                disk,
            )
            smoke = step(build, "Boot the disk and reach tilo-vmd over SSH")
            self.assertIn('qemu-img create -f qcow2 -F qcow2 -b "$TILO_DISK"', smoke)
            self.assertIn('./os/dev/check-dev-vm.sh --arch "$ARCH" --base-digest "$BASE_CHILD_DIGEST"', smoke)
            order = [
                "Build the dev image FROM the verified base",
                "Install the dev image to a bootable disk",
                "Publish unvalidated installed disk candidate",
                "Boot the disk and reach tilo-vmd over SSH",
                "Publish this architecture's dev image and disk",
            ]
            positions = [build.index(f"- name: {name}") for name in order]
            self.assertEqual(positions, sorted(positions))
        with self.subTest(phase='candidate is explicit and cannot commit a validated tag'):
            build = job(self.source, "build")
            candidate = step(build, "Publish unvalidated installed disk candidate")
            self.assertIn("./os/dev/publish-dev-disk-candidate.sh", candidate)
            self.assertIn("TILO_PACKAGE_TOKEN: ${{ secrets.REGISTRY_TOKEN }}", candidate)
            self.assertIn('echo "DEV_IMAGE_DIGEST=$digest" >> "$GITHUB_ENV"', build)
            self.assertIn('echo "DEV_IMAGE_DIGEST=$actual" >> "$GITHUB_ENV"', build)
            source = CANDIDATE.read_text(encoding="utf-8")
            self.assertIn("https://tilo.sh/dev/disk-candidate/v1", source)
            self.assertIn('validation:"unvalidated"', source)
            self.assertIn("/tilo-vm-disk-candidate/$version", source)
            self.assertNotIn("/tilo-vm-disk/$DEV_TAG", source)
            for field in ("sourceRevision:", "diskToolingRevision:", "imageDigest:", "baseChildDigest:", "sha256:"):
                self.assertIn(field, source)
            self.assertLess(source.index('"$repo/tools/upload_immutable_packages.sh" "$uploads"'),
                            source.index('manifest=candidate.json'))
        with self.subTest(phase='uploads use the package token without an integration audience'):
            publish = step(job(self.source, "build"), "Publish this architecture's dev image and disk")
            self.assertIn("TILO_PACKAGE_TOKEN: ${{ secrets.REGISTRY_TOKEN }}", publish)
            # The token must reach the uploader only through the step environment,
            # never on a command line.
            self.assertNotIn('TILO_PACKAGE_TOKEN="', self.source)
            self.assertNotIn("TILO_VM_DISK_RELEASE_AUDIENCE", self.source)
            self.assertNotIn("ACTIONS_ID_TOKEN_REQUEST", self.source)
            self.assertNotIn("enable-openid-connect", self.source)
        with self.subTest(phase='dev images are never release signed'):
            for source in (self.source, PUBLISH.read_text(encoding="utf-8"), CHECK.read_text(encoding="utf-8")):
                self.assertNotIn("COSIGN_KEY_REF", source)
                self.assertNotRegex(source, r"cosign (?:sign|attest)")
        with self.subTest(phase='base trust follows the repository'):
            plan = job(self.source, "plan")
            self.assertIn("trust=release", plan)
            self.assertIn("trust=integration", plan)
            self.assertIn("suffix=-ci", plan)
            self.assertIn('[[ $trust == release || $tag == *-ci ]]', plan)
            # Room for the -<arch> suffix of the child tag within 128 characters.
            self.assertIn("{0,119}$", plan)


class PublishScriptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = PUBLISH.read_text(encoding="utf-8")

    def test_publish_script_commits_each_architecture_immutably_in_order(self) -> None:
        with self.subTest(phase='writes child index disks manifest then latest'):
            order = [
                'podman push --digestfile "$digest_file" "$DEV_IMAGE" "docker://$child_tag"',
                'podman manifest push --all "$list" "docker://$index_tag"',
                'TILO_PACKAGE_UPLOAD_WORKERS=2 "$REPO/tools/upload_immutable_packages.sh" "$uploads"',
                '"$REPO/tools/upload_immutable_packages.sh" "$uploads"\nrm -f "$uploads"',
                'skopeo copy --multi-arch index-only --preserve-digests \\\n'
                '    "docker://$REPOSITORY@$target" "docker://$latest"',
            ]
            positions = [self.source.index(marker) for marker in order]
            self.assertEqual(positions, sorted(positions))
        with self.subTest(phase='each architecture commits its own manifest'):
            self.assertIn('manifest_file="tilo-vm-disk-$ARCH.json"', self.source)
            self.assertIn('child_tag="$REPOSITORY:$DEV_TAG-$ARCH"', self.source)
            # A committed architecture is never republished.
            self.assertIn('already publishes $ARCH', self.source)
            for field in ("tag:", "arch:", "sourceRevision:", "imageDigest:", "baseDigest:", "baseTrust:", "disks:"):
                self.assertIn(field, self.source)
        with self.subTest(phase='index is rebuilt from children until a read agrees'):
            self.assertIn('for arch in x86_64 aarch64; do', self.source)
            self.assertIn('== "$SOURCE_REVISION" ]] || continue', self.source)
            self.assertIn('another $ARCH lane is publishing', self.source)
            self.assertIn('did not converge on the published children', self.source)
        with self.subTest(phase='integration bases publish only ci tags'):
            self.assertIn("integration:registry.tilo.so/tilo/tilo-ci) [[ $DEV_TAG == *-ci ]]", self.source)
            self.assertIn("release:registry.tilo.so/tilo/tilo) ;;", self.source)
        with self.subTest(phase='latest moves only forward on the channel'):
            self.assertIn("[[ $CHANNEL == true ]] || exit 0", self.source)
            self.assertIn('merge-base --is-ancestor "$1" "$SOURCE_REVISION"', self.source)
        with self.subTest(phase='disks use immutable generic packages'):
            self.assertIn("-O qcow2 -c -o compression_type=zstd", self.source)
            self.assertIn("-O vhdx -o subformat=dynamic", self.source)
            self.assertIn('package="$package_api/tilo-vm-disk/$DEV_TAG"', self.source)
            self.assertIn("*'.svc.cluster.local'*", self.source)
        with self.subTest(phase='already-compressed CI disk is published without a second copy'):
            self.assertIn('"compression-type" == "zstd"', self.source)
            self.assertIn('ln -- "$SOURCE_DISK" "$out/$file"', self.source)
            candidate = CANDIDATE.read_text(encoding="utf-8")
            self.assertIn('sha256sum "$SOURCE_DISK"', candidate)
            self.assertNotIn('qemu-img convert', candidate)
        with self.subTest(phase='disk manifest records separate tooling revision'):
            self.assertIn("DISK_TOOLING_REVISION", self.source)
            self.assertIn('[[ $DISK_TOOLING_REVISION =~ ^[0-9a-f]{40}$ ]]', self.source)
            self.assertIn("diskToolingRevision: $tooling", self.source)


class DevVmCheckTests(unittest.TestCase):
    def test_tilo_vmd_answers_as_root_for_the_expected_arch_and_base(self) -> None:
        source = CHECK.read_text(encoding="utf-8")
        self.assertIn('"method":"hello"', source)
        self.assertIn("sudo -n tilo-vmd rpc", source)
        self.assertIn(".result.arch == $arch", source)
        self.assertIn(".result.base.image_digest == $base", source)


class OsLaneGatesAndPublishesTheDevDiskTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = OS_RELEASE.read_text(encoding="utf-8")
        self.lane = job(self.source, "build-and-push")

    def test_os_lane_gates_and_publishes_its_own_dev_disk(self) -> None:
        with self.subTest(phase='no run waits on every lane before a dev disk'):
            self.assertNotIn("dispatch-dev-disk:", self.source)
        with self.subTest(phase='integration gates the dev image built from this lane'):
            gate = step(self.lane, "Reseal, install, boot, and stage the verified image")
            self.assertIn("DEV_GATE: ${{ steps.release-mode.outputs.dev-gate }}", gate)
            self.assertIn('TILO_DEV_BASE="$dev_base"', gate)
            self.assertIn("./os/dev/build-dev-image.sh", gate)
            # No registry round trip: the base is this lane's own image.
            self.assertIn('podman tag "$first_tag" "$dev_base"', gate)
            # Kata lanes import, build and install on a native overlay store,
            # before the image enters Podman at all.
            self.assertIn('CONTAINERS_STORAGE_CONF="$(./os/dev/ci-native-podman-store.sh)"', gate)
            self.assertLess(
                gate.index("ci-native-podman-store.sh"),
                gate.index('"containers-storage:$first_tag"'),
            )
            self.assertNotIn("podman pull", gate)
            # One install per lane: the prod install only in the other branch.
            dev = gate.index("if [[ $DEV_GATE == true ]]; then")
            prod = gate.index("./os/dev/build-image.sh", dev)
            self.assertLess(gate.index("          else\n", dev), prod)
            # Same desktop gate, then tilo-vmd.
            self.assertIn("./os/dev/vm.sh ssh /usr/libexec/tilo/native-atspi-smoke.sh lock", gate)
            self.assertLess(
                gate.index("native-atspi-smoke.sh lock"),
                gate.index('./os/dev/check-dev-vm.sh --arch'),
            )
            # The slow-frame sign-in ends the session, so it comes last.
            slow = gate.index("./os/dev/test-slow-frame-launch.sh")
            self.assertLess(gate.index("./os/tests/booted_app_update.sh"), slow)
            self.assertLess(slow, gate.index("          else\n", slow))
            self.assertIn('qemu-img create -f qcow2 -F qcow2 -b "$DEV_OUTPUT/qcow2/disk.qcow2" "$TILO_DISK"', gate)
        with self.subTest(phase='the lane publishes its own architecture'):
            publish = step(self.lane, "Publish this architecture's dev image and disk")
            self.assertIn("steps.release-mode.outputs.dev-gate == 'true'", publish)
            self.assertIn("steps.dev-image.outcome == 'success'", publish)
            self.assertIn("./os/dev/publish-dev-disk.sh", publish)
            self.assertIn("BASE_REPOSITORY: registry.tilo.so/tilo/tilo-ci", publish)
            self.assertIn("BASE_TRUST: integration", publish)
            self.assertIn("DEV_TAG: ${{ steps.release-mode.outputs.dev-series }}-ci", publish)
            self.assertIn("DISK_TOOLING_REVISION: ${{ env.TILO_IMAGE_SOURCE }}", publish)
            self.assertLess(
                self.lane.index("- name: Reseal, install, boot, and stage the verified image"),
                self.lane.index("- name: Publish this architecture's dev image and disk"),
            )
        with self.subTest(phase='dev trust image is pushed unsigned'):
            push = step(self.lane, "Publish the dev-trust integration image")
            self.assertIn("steps.release-mode.outputs.dev-series != ''", push)
            self.assertIn("continue-on-error: true", push)
            self.assertIn('ref="registry.tilo.so/tilo/tilo-ci:$DEV_SERIES-$ARCH"', push)
            self.assertNotRegex(push, r"cosign (?:sign|attest)")
            mode = step(self.lane, "Select build, validation, or publication mode")
            self.assertIn('series="main-${TILO_IMAGE_SOURCE:0:7}"', mode)
            self.assertIn('echo "dev-channel=$([[ $GITHUB_REF == refs/heads/main ]]', mode)
        with self.subTest(phase='publication requests its architecture from the signed child'):
            request = step(self.lane, "Request this architecture's dev disk from the signed child")
            self.assertIn("steps.release-mode.outputs.publish == 'true'", request)
            self.assertIn("continue-on-error: true", request)
            self.assertIn("IMAGE: registry.tilo.so/tilo/tilo@${{ steps.release.outputs.digest }}", request)
            self.assertIn("architectures:$arch", request)
            self.assertIn("actions/workflows/dev-disk.yml/dispatches", request)
            self.assertLess(
                self.lane.index("- name: Verify and describe the native release child"),
                self.lane.index("- name: Request this architecture's dev disk from the signed child"),
            )



if __name__ == "__main__":
    unittest.main()
