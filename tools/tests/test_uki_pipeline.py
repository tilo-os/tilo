import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


check = load("check_uki_pipeline_under_test", REPO / "tools/policy/check_uki_pipeline.py")
FILES = (
    "os/Containerfile",
    "os/Containerfile.board",
    "os/Containerfile.board.dockerignore",
    "os/Containerfile.dev-installer-base",
    "os/dev/uki-tools.packages",
    "os/dev/derive-rpm-package-set.py",
    ".forgejo/workflows/os-dev-installer.yml",
    # derive-rpm-package-set.py's other sources, so the fixture's snapshot is the real one.
    "os/Containerfile.dev",
    "os/Containerfile.host",
    "os/dev/composefs/Containerfile",
    "os/containers/tilo-relay/Containerfile",
    "os/Containerfile.env-runtime",
)


class UkiPipelineTests(unittest.TestCase):
    def fixture(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for name in FILES:
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / name, root / name)
        return root

    def edit(self, path: Path, old: str, new: str) -> None:
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def findings(self, root: Path) -> str:
        return "\n".join(str(finding) for finding in check.audit(root))

    def test_repository_and_unchanged_fixture_pass(self):
        self.assertEqual(self.findings(REPO), "")
        self.assertEqual(self.findings(self.fixture()), "")

    def test_a_seal_input_missing_from_one_image_is_reported(self):
        root = self.fixture()
        board = root / "os/Containerfile.board"
        self.edit(board, "ARG TILO_SEAL_GENERATION=1\nENV TILO_SEAL_GENERATION=${TILO_SEAL_GENERATION}\n", "")
        found = self.findings(root)
        self.assertIn("os/Containerfile.board:", found)
        self.assertIn("not the seal inputs", found)

    def test_a_package_added_to_one_uki_tools_stage_is_reported(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.dev-installer-base",
                  "    | xargs dnf install -y --setopt=reposdir=/repo \\\n",
                  "    | xargs dnf install -y --setopt=reposdir=/repo python3-pillow \\\n")
        found = self.findings(root)
        self.assertIn("os/Containerfile.dev-installer-base:", found)
        self.assertIn("differs from os/Containerfile's", found)
        self.assertIn("python3-pillow", found)

    def test_a_package_named_inline_in_the_reference_stage_is_reported(self):
        root = self.fixture()
        for name in ("os/Containerfile", "os/Containerfile.board", "os/Containerfile.dev-installer-base"):
            self.edit(root / name, "    | xargs dnf install -y --setopt=reposdir=/repo \\\n",
                      "    | xargs dnf install -y --setopt=reposdir=/repo sbsigntools \\\n")
        found = self.findings(root)
        self.assertIn("names packages inline: sbsigntools", found)
        self.assertNotIn("differs from os/Containerfile's", found)

    def test_a_tool_step_added_to_one_copy_is_reported(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.board",
                  "COPY os/dev/uki.sh os/dev/uki-hardware-profile.py",
                  "RUN true\nCOPY os/dev/uki.sh os/dev/uki-hardware-profile.py")
        self.assertIn("the `uki-tools` stage differs", self.findings(root))

    def test_comments_and_line_wrapping_are_not_differences(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.board",
                  "    | xargs dnf install -y --setopt=reposdir=/repo \\\n",
                  "    # a note between continuation lines\n    | xargs dnf install -y \\\n"
                  "        --setopt=reposdir=/repo \\\n")
        self.assertEqual(self.findings(root), "")

    def test_a_uki_stage_that_skips_uki_sh_or_its_kernel_is_reported(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.dev-installer-base",
                  "    --mount=type=bind,from=split,source=/kernel,target=/kernel,rw \\\n"
                  "    bash /usr/local/bin/uki.sh\n",
                  "    bash /usr/local/bin/uki.sh && true\n")
        found = self.findings(root)
        self.assertIn("os/Containerfile.dev-installer-base:", found)
        self.assertIn("--mount=type=bind,from=split,source=/kernel,target=/kernel,rw", found)

    def test_a_layer_added_after_the_split_is_reported(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.board", "COPY --from=uki /out/*.efi /boot/EFI/Linux/\n",
                  "COPY --from=uki /out/*.efi /boot/EFI/Linux/\nRUN touch /etc/late\n")
        self.assertIn("`final` carries `RUN touch /etc/late`", self.findings(root))

    def test_a_split_stage_that_does_not_end_in_the_split_is_reported(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.dev-installer-base",
                  "    && bootc container split-kernel-and-rootfs --rootfs / --output /kernel\n",
                  "    && bootc container split-kernel-and-rootfs --rootfs / --output /kernel\nRUN touch /late\n")
        self.assertIn("the `split` stage does not end with", self.findings(root))

    def test_hand_assembled_contexts_must_carry_the_package_list(self):
        root = self.fixture()
        self.edit(root / "os/Containerfile.board.dockerignore", "!os/dev/uki-tools.packages\n", "")
        self.edit(root / ".forgejo/workflows/os-dev-installer.yml", "cp os/dev/uki-tools.packages ", "cp ")
        found = self.findings(root)
        self.assertIn("board's sparse context does not include `os/dev/uki-tools.packages`", found)
        self.assertIn("adapter context does not copy `os/dev/uki-tools.packages`", found)

    def test_the_package_list_feeds_the_snapshot(self):
        root = self.fixture()
        self.edit(root / "os/dev/derive-rpm-package-set.py", '    "os/dev/uki-tools.packages",\n', "")
        self.assertIn("is not a snapshot package source", self.findings(root))
        derived = subprocess.run([sys.executable, str(REPO / "os/dev/derive-rpm-package-set.py"),
                                  "--arch", "aarch64"], check=True, capture_output=True, text=True).stdout
        for package in ("systemd-ukify", "systemd-boot-unsigned", "binutils"):
            self.assertIn(package, derived.splitlines())


if __name__ == "__main__":
    unittest.main()
