"""Tests for tools/policy/check_media_stack.py."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_media_stack", ROOT / "tools/policy/check_media_stack.py"
)
assert SPEC and SPEC.loader
check_media_stack = importlib.util.module_from_spec(SPEC)
sys.modules["check_media_stack"] = check_media_stack
SPEC.loader.exec_module(check_media_stack)


def write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(text), encoding="utf-8")


class MediaStackPolicyTests(unittest.TestCase):
    def test_codec_crates_are_reported_in_every_dependency_form(self) -> None:
        with self.subTest(phase='the repository carries no in tree codec crate'):
            self.assertEqual(check_media_stack.check(ROOT), [])
        with self.subTest(phase='a direct dependency is reported'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                write(root, "Cargo.toml", '[workspace]\nmembers = ["a"]\n')
                write(
                    root,
                    "a/Cargo.toml",
                    """
                [package]
                name = "a"
                [dependencies]
                openh264 = { version = "0.9", features = ["source"] }
                """,
                )
                found = check_media_stack.check(root)
                self.assertEqual([(v.path, v.name) for v in found], [("a/Cargo.toml", "openh264")])
        with self.subTest(phase='renamed optional and target dependencies are reported'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                write(
                    root,
                    "Cargo.toml",
                    """
                [workspace.dependencies]
                av = { package = "ffmpeg-next", version = "8" }
                [target.'cfg(unix)'.dependencies]
                ffmpeg-sys-next = "8"
                """,
                )
                names = sorted(v.name for v in check_media_stack.check(root))
                self.assertEqual(names, ["ffmpeg-next", "ffmpeg-sys-next"])
        with self.subTest(phase='a transitive package in the lock is reported'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                write(root, "Cargo.toml", "[workspace]\n")
                write(
                    root,
                    "Cargo.lock",
                    """
                version = 4
                [[package]]
                name = "openh264-sys2"
                version = "0.9.6"
                """,
                )
                found = check_media_stack.check(root)
                self.assertEqual([(v.path, v.name) for v in found], [("Cargo.lock", "openh264-sys2")])


class CiscoOpenH264ImageTests(unittest.TestCase):
    def found(self, files: dict[str, str]) -> list[tuple[str, str]]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, text in files.items():
                write(root, rel, text)
            return [(v.path, v.name) for v in check_media_stack.image_violations(root)]

    def test_the_repository_ships_no_cisco_openh264_and_names_only_real_environments(self) -> None:
        self.assertEqual(check_media_stack.image_violations(ROOT), [])
        self.assertEqual(check_media_stack.stale_environments(ROOT), [])
        self.assertTrue(check_media_stack.image_sources(ROOT))

    def test_installing_ciscos_packages_by_name_is_reported(self) -> None:
        for package in check_media_stack.CISCO_PACKAGES:
            with self.subTest(package=package):
                found = self.found({
                    "crates/x/container/Containerfile": f"""
                    FROM registry.fedoraproject.org/fedora-minimal:44
                    RUN microdnf install -y --setopt=install_weak_deps=0 \\
                          gstreamer1 {package} \\
                     && microdnf clean all
                    """,
                })
                self.assertEqual(found, [("crates/x/container/Containerfile", package)])
        found = self.found({
            "os/dev/tools.packages": "glibc\nopenh264\n",
            "os/scripts/setup.sh": "rpm -Uvh ./openh264-2.6.0-1.fc44.x86_64.rpm\n",
        })
        self.assertEqual(found, [("os/dev/tools.packages", "openh264"), ("os/scripts/setup.sh", "openh264")])

    def test_mentions_that_install_nothing_pass(self) -> None:
        found = self.found({
            "os/Containerfile.rpms": """
            FROM quay.io/fedora/fedora-bootc:44 AS configured-base
            # dnf install gstreamer1-plugin-openh264 would pull Cisco's openh264.
            RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo
            RUN dnf install -y --disablerepo=fedora-cisco-openh264 noopenh264 libopenh264-free
            """,
            "os/scripts/verify.sh": "forbidden='^(openh264(-.+)?|gstreamer1-plugin-openh264)$'\n",
            "os/tests/probe.sh": "gst-inspect-1.0 openh264enc && rpm -q openh264\n",
        })
        self.assertEqual(found, [])

    def test_enabling_ciscos_repository_is_reported(self) -> None:
        for command in (
            "dnf install -y --enablerepo=fedora-cisco-openh264 ffmpeg-free",
            "dnf config-manager setopt fedora-cisco-openh264.enabled=1",
            "dnf5 config-manager --enable fedora-cisco-openh264",
        ):
            with self.subTest(command=command):
                found = self.found({"os/Containerfile.x": f"FROM fedora:44\nRUN {command}\n"})
                self.assertEqual(found, [("os/Containerfile.x", "fedora-cisco-openh264")])

    def test_running_the_ci_dependency_script_in_an_image_is_reported(self) -> None:
        text = """
        FROM fedora:44
        RUN --mount=type=bind,source=dist/ci/install-build-deps.sh,target=/tmp/deps.sh,ro \\
            bash /tmp/deps.sh fedora test
        """
        self.assertEqual(self.found({"os/Containerfile.x": text}),
                         [("os/Containerfile.x", "install-build-deps")])
        self.assertEqual(self.found({"os/Containerfile.builder": text}), [])
        commented = "FROM fedora:44\n# The toolchain comes from dist/ci/install-build-deps.sh's rustup.\n"
        self.assertEqual(self.found({"os/Containerfile.x": commented}), [])

    def test_a_final_stage_on_the_ci_builder_image_is_reported(self) -> None:
        builder = "registry.tilo.so/tilo/tilo-builder@sha256:" + "0" * 64
        on_builder = {
            "direct": f"FROM {builder}\nCOPY bin/ /usr/local/bin/\n",
            "through an ARG default": f"ARG BUILD_IMAGE={builder}\nFROM ${{BUILD_IMAGE}}\n",
            "through a stage alias": f"ARG B={builder}\nFROM --platform=linux/amd64 $B AS build\n"
                                     "RUN make\nFROM build\n",
        }
        for case, text in on_builder.items():
            with self.subTest(case=case):
                self.assertEqual(self.found({"os/Containerfile.x": text}),
                                 [("os/Containerfile.x", "tilo/tilo-builder")])
        outputs_only = f"ARG BUILD_IMAGE={builder}\nFROM ${{BUILD_IMAGE}} AS build\nRUN make\n" \
                       "FROM scratch\nCOPY --from=build /out/tool /tool\n"
        self.assertEqual(self.found({"os/Containerfile.x": outputs_only}), [])

    def test_a_missing_environment_is_stale(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stale = sorted(v.path for v in check_media_stack.stale_environments(Path(tmp)))
        self.assertEqual(stale, sorted([*check_media_stack.CISCO_ENVIRONMENTS,
                                        *check_media_stack.LIVE_REPOSITORY_IMAGES]))


class LiveRepositoryImageTests(unittest.TestCase):
    IMAGE = "crates/hosting/tilo-room-service/container/Containerfile"
    GATED = """
    FROM registry.fedoraproject.org/fedora-minimal:44 AS runtime
    RUN --mount=type=bind,source=verify-no-cisco-openh264.sh,target=/run/tilo-verify-no-cisco-openh264.sh,ro \\
      rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo \\
      && microdnf install -y gstreamer1 \\
      && bash /run/tilo-verify-no-cisco-openh264.sh
    COPY bin/ /usr/local/bin/
    """

    def found(self, text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root, self.IMAGE, text)
            return [v.name for v in check_media_stack.live_repository_violations(root)]

    def test_the_repositorys_live_repository_images_are_gated(self) -> None:
        self.assertEqual(check_media_stack.live_repository_violations(ROOT), [])
        for image in check_media_stack.LIVE_REPOSITORY_IMAGES:
            with self.subTest(image=image):
                text = (ROOT / image).read_text(encoding="utf-8")
                self.assertTrue(check_media_stack.PACKAGE_INSTALL.search(text),
                                "the image no longer installs packages; drop it from "
                                "LIVE_REPOSITORY_IMAGES rather than keep a rule that observes nothing")

    def test_a_removal_before_the_install_and_a_gate_after_it_pass(self) -> None:
        self.assertEqual(self.found(self.GATED), [])
        split = """
        FROM fedora-bootc:44
        RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo \\
         && dnf install -y flatpak
        COPY rootfs/ /
        RUN --mount=type=bind,source=os/scripts/verify-no-cisco-openh264.sh,target=/run/v.sh,ro \\
            true && bash /run/verify-no-cisco-openh264.sh && bootc container lint
        """
        self.assertEqual(self.found(split), [])

    def test_installing_without_removing_the_repository_first_is_reported(self) -> None:
        cases = {
            "no removal": self.GATED.replace("rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo", "true"),
            "removal after the install": """
            FROM fedora:44
            RUN dnf install -y flatpak && bash /run/verify-no-cisco-openh264.sh
            RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo
            """,
            "removal in an earlier stage": """
            FROM fedora:44 AS base
            RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo
            FROM fedora:44
            RUN dnf install -y flatpak && bash /run/verify-no-cisco-openh264.sh
            """,
        }
        for case, text in cases.items():
            with self.subTest(case=case):
                self.assertEqual(self.found(text), ["fedora-cisco-openh264"])

    def test_installing_without_the_resolved_gate_afterwards_is_reported(self) -> None:
        cases = {
            "no gate": self.GATED.replace("\\\n      && bash /run/tilo-verify-no-cisco-openh264.sh", ""),
            "gate before the last install": """
            FROM fedora:44
            RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo && bash /run/verify-no-cisco-openh264.sh
            RUN dnf install -y flatpak
            """,
            "only the snapshot form": """
            FROM fedora:44
            RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo && dnf install -y flatpak \\
             && bash /run/verify-no-cisco-openh264.sh --repo /repo
            """,
            "only in a comment": """
            FROM fedora:44
            RUN rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo && dnf install -y flatpak
            # RUN bash /run/verify-no-cisco-openh264.sh
            """,
        }
        for case, text in cases.items():
            with self.subTest(case=case):
                self.assertEqual(self.found(text), ["verify-no-cisco-openh264"])


if __name__ == "__main__":
    unittest.main()
