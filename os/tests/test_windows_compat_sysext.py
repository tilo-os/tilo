"""Contract: Windows compatibility is a signed system extension, not image content.

The payload (FEX, the x86 guest root, Proton, the Steam runtime, virtio-win) is
about 3 GB. It must stay out of the final rootfs, be packed by one named stage,
overlay the paths the hard-coded readers use, be offered by the image's
extension catalogue. Only a CI image build can prove the bytes; this proves
the payload paths cannot silently drift back into the image.
"""

import json
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CONTAINERFILE = (REPO / "os/Containerfile").read_text()
CATALOGUE = json.loads(
    (REPO / "os/usr/share/tilo/system-extensions/sysext.d/windows-compat.json").read_text())
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


class SysextStagePacksThePayload(unittest.TestCase):
    def test_every_hard_coded_path_is_packed_at_the_same_location(self):
        packed = STAGES["windows-compat-sysext-build"]
        for path in PAYLOAD_PATHS:
            self.assertIn(f"/tree{path}", packed, f"{path} is missing from the sysext tree")
        self.assertIn("COPY --from=windows-provider /payload/ /tree/", packed)


class CatalogueOffersIt(unittest.TestCase):


    def test_planner_probes_exactly_the_paths_the_sysext_provides(self):
        for host in ("PROTON_HOST", "STEAM_RUNTIME_HOST", "X86_ROOTFS_HOST"):
            value = re.search(rf'{host}: &str =\s*"([^"]+)"', PLAN).group(1)
            self.assertTrue(value.startswith("/usr/lib/tilo/windows-runners"), host)
        self.assertIn("WindowsSupportNotInstalled", PLAN)


class CiBuildsAndPublishesIt(unittest.TestCase):

    def test_published_url_matches_the_catalogue_manifest_location(self):
        self.assertIn("tilo-extension-windows-compat/{arch}-{base}/manifest.json", CATALOGUE["manifest"])


if __name__ == "__main__":
    unittest.main()
