"""Contract: Windows compatibility is a signed system extension, not image content.

The payload (FEX, the x86 guest root, Proton, the Steam runtime, virtio-win) is
about 3 GB. It must stay out of the final rootfs, be packed by one named stage,
overlay the paths the hard-coded readers use, be offered by the image's
extension catalogue with a size disclosure, and have a build, a manifest and a
publication wired in CI. Only a CI image build can prove the bytes; this proves
the wiring cannot silently drift back.
"""

import json
import re
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/ci"))
import t_matrix  # noqa: E402
CONTAINERFILE = (REPO / "os/Containerfile").read_text()
CATALOGUE = json.loads(
    (REPO / "os/usr/share/tilo/system-extensions/sysext.d/windows-compat.json").read_text())
BUILD_IMAGE = (REPO / ".forgejo/workflows/build-image.yml").read_text()
# t's extension-manifests-{x86_64,aarch64} and publish-extension-manifests checks run this.
MANIFESTS = (REPO / "tools/ci/publish/extension-manifests.sh").read_text()
BUILD_SCRIPT = (REPO / "os/dev/build-windows-compat-sysext.sh").read_text()
# /usr/libexec/tilo-extension, built from this crate.
HELPER = "".join(path.read_text() for path in sorted((REPO / "crates/system/extension-helper/src").glob("*.rs")))
PLAN = (REPO / "crates/windows/compat-service/src/plan.rs").read_text()

# What the compatibility service and the Windows VM name by absolute path.
PAYLOAD_PATHS = (
    "/usr/lib/tilo/windows-runners/proton/",
    "/usr/lib/tilo/windows-runners/steam-runtime/",
    "/usr/share/virtio-win/virtio-win.iso",
    "/usr/share/virtio-win/installer/virtio-win-guest-tools.exe",
)


def stages(text):
    """Stage name -> its instruction text (continuations joined)."""
    result, name, body = {}, None, []
    for line in text.replace("\\\n", " ").splitlines():
        match = re.match(r"FROM\s+\S+(?:\s+AS\s+(\S+))?", line)
        if match:
            if name:
                result[name] = "\n".join(body)
            name, body = match.group(1) or f"stage{len(result)}", []
        elif name:
            body.append(line)
    if name:
        result[name] = "\n".join(body)
    return result


STAGES = stages(CONTAINERFILE)


class FinalImageCarriesNoPayload(unittest.TestCase):
    def final_stage(self):
        return next(body for name, body in STAGES.items()
                    if "LABEL org.tilo.with-windows" in body)

    def test_no_copy_into_the_payload_paths(self):
        final = self.final_stage()
        for path in ("/usr/lib/tilo/windows-runners", "/usr/share/virtio-win"):
            self.assertNotRegex(final, rf"(?m)^COPY .* {re.escape(path)}",
                                f"{path} must come from the sysext, not the rootfs")

    def test_provider_stage_copies_only_the_build_record_into_the_image(self):
        final = self.final_stage()
        self.assertIn("COPY --from=windows-provider /out/usr/ /usr/", final)
        for name in ("windows-provider-aarch64-1", "windows-provider-x86_64-1",
                     "windows-provider-off"):
            self.assertNotIn("/out/usr/lib/tilo/windows-runners", STAGES[name])
        arm = STAGES["windows-provider-aarch64-1"]
        self.assertIn("cp -a /fex/usr/. /payload/usr/", arm)
        self.assertIn("cp -a /guest/rootfs /payload/usr/lib/tilo/windows-runners/x86", arm)
        self.assertNotRegex(arm, r"cp -a /fex/usr/\. /out/")

    def test_small_launcher_pieces_stay_in_the_base(self):
        final = self.final_stage()
        self.assertIn("/usr/libexec/tilo-umu-run", final)
        self.assertIn("/usr/share/tilo/windows/catalogues/", final)


class SysextStagePacksThePayload(unittest.TestCase):
    def test_every_hard_coded_path_is_packed_at_the_same_location(self):
        packed = STAGES["windows-compat-sysext-build"]
        for path in PAYLOAD_PATHS:
            self.assertIn(f"/tree{path}", packed, f"{path} is missing from the sysext tree")
        self.assertIn("COPY --from=windows-provider /payload/ /tree/", packed)

    def test_the_stage_validates_the_native_payload_and_builds_the_image(self):
        packed = STAGES["windows-compat-sysext-build"]
        self.assertIn("windows_provider.py host --manifest /provider.json --root /tree", packed)
        self.assertIn("build-windows-compat-sysext.sh /tree /out/tilo-windows-compat.raw", packed)
        self.assertRegex(STAGES["windows-compat-sysext"], r"COPY --from=windows-compat-sysext-build /out/ /")

    def test_the_build_script_names_the_part_and_restricts_to_usr(self):
        self.assertIn("part=tilo-windows-compat", BUILD_SCRIPT)
        self.assertIn("extension-release.$part", BUILD_SCRIPT)
        self.assertIn("only /usr may travel", BUILD_SCRIPT)
        for key in ("ID=", "VERSION_ID=", "ARCHITECTURE=", "SYSEXT_SCOPE=system"):
            self.assertIn(key, BUILD_SCRIPT)


class CatalogueOffersIt(unittest.TestCase):
    def test_entry_names_the_part_and_discloses_the_size(self):
        self.assertEqual(CATALOGUE["id"], "windows-compat")
        self.assertEqual(CATALOGUE["parts"], ["tilo-windows-compat"])
        self.assertEqual(CATALOGUE["hosts"], ["code.tilo.so"])
        size = CATALOGUE["size"]
        self.assertGreaterEqual(size["installed_bytes"], 3_000_000_000)
        self.assertLessEqual(size["download_bytes"], size["installed_bytes"])

    def test_the_helper_installs_prebuilt_images_and_only_those_it_verified(self):
        self.assertIn('pub const RAW_FORMAT: &str = "sysext-raw";', HELPER)
        self.assertIn("fn build_raw_part(", HELPER)
        self.assertIn("a sysext image part is exactly one artifact", HELPER)

    def test_planner_probes_exactly_the_paths_the_sysext_provides(self):
        for host in ("PROTON_HOST", "STEAM_RUNTIME_HOST", "X86_ROOTFS_HOST"):
            value = re.search(rf'{host}: &str =\s*"([^"]+)"', PLAN).group(1)
            self.assertTrue(value.startswith("/usr/lib/tilo/windows-runners"), host)
        self.assertIn("WindowsSupportNotInstalled", PLAN)


class CiBuildsAndPublishesIt(unittest.TestCase):
    def test_image_workflow_exports_the_target_and_publishes_it(self):
        self.assertIn("target: windows-compat-sysext", BUILD_IMAGE)
        self.assertIn("name: windows-compat-sysext-${{ matrix.arch }}", BUILD_IMAGE)
        self.assertIn("publish-windows-compat:", BUILD_IMAGE)
        self.assertIn("tilo-windows-compat-image/$arch-$sha256/tilo-windows-compat.raw", BUILD_IMAGE)
        # The descriptor the manifest job pins is checked against the bytes first.
        self.assertIn("does not match its descriptor", BUILD_IMAGE)

    def test_manifest_script_pins_it_for_the_promoted_image(self):
        self.assertIn("tools/extensions/sysext_image.py manifest --id windows-compat", MANIFESTS)
        self.assertIn("tilo-windows-compat-image/$arch-$revision/descriptor.json", MANIFESTS)
        self.assertIn("tilo-windows-compat-image/$arch-$sha256/tilo-windows-compat.raw", MANIFESTS)
        runs = [check.get("run", "") for check in t_matrix.checks(REPO)]
        for arch in ("x86_64", "aarch64"):
            self.assertTrue(any(run.startswith(f"bash tools/ci/publish/extension-manifests.sh resolve {arch} ")
                                for run in runs), arch)
        self.assertTrue(any(run.startswith("bash tools/ci/publish/extension-manifests.sh publish ") for run in runs))

    def test_published_url_matches_the_catalogue_manifest_location(self):
        self.assertIn("tilo-extension-windows-compat/{arch}-{base}/manifest.json", CATALOGUE["manifest"])


if __name__ == "__main__":
    unittest.main()
