"""Contract of .forgejo/workflows/app-sdk.yml."""

from __future__ import annotations

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SOURCE = (ROOT / ".forgejo/workflows/app-sdk.yml").read_text()


def block(name: str, indent: int) -> str:
    """The lines of a mapping key at `indent` spaces, up to the next sibling."""
    pad = " " * indent
    match = re.search(rf"^{pad}{re.escape(name)}:\n((?:{pad} .*\n|\s*\n)*)", SOURCE, re.M)
    assert match, name
    return match.group(1)


TRIGGERS = block("on", 0)
JOBS = block("jobs", 0)


def job(name: str) -> str:
    match = re.search(rf"^  {name}:\n((?:    .*\n|\s*\n)*)", JOBS, re.M)
    assert match, name
    return match.group(1)


class AppSdkWorkflow(unittest.TestCase):
    def test_web_builds_pinned_moq_once_and_keeps_both_bundle_licenses(self) -> None:
        web = job("web")
        prepare = 'python3 tools/ci/prepare_sdk_moq.py prepare'
        self.assertEqual(web.count(prepare), 1)
        self.assertLess(web.index(prepare), web.index('name: Desktop bundle (dev profile)'))
        for app in ('calculator', 'chat'):
            self.assertIn('python3 tools/ci/prepare_sdk_moq.py license '
                          f'target/tilo-app/web/{app}/bundle', web)
        self.assertIn('target/debug/tilo-app build --target web --bundle desktop apps/calculator', web)
        self.assertIn('target/debug/tilo-app build apps/chat --platform web', web)
        self.assertIn('Compiled Chat browser disconnect and retry', web)
        self.assertIn('Compiled Chat browser call consent and lifecycle', web)
        self.assertIn('Mount every registered app', web)
        self.assertIn('Desktop navigation by wheel, top bar and shortcuts', web)
        self.assertNotIn(prepare, job('web-size'))  # One release bundle already needs only one setup.

    def test_linux_selection_does_not_change_browser_or_manual_gates(self) -> None:
        self.assertIn("needs.builder.outputs.linux_affected == 'true'", job('linux'))
        for name in ('web', 'web-size'):
            self.assertIn("needs.builder.outputs.affected == 'true'", job(name))
            self.assertNotIn('linux_affected', job(name))
        builder = job('builder')
        self.assertIn('linux_affected: ${{ steps.affected.outputs.linux_affected }}', builder)
        self.assertIn('echo linux_affected=true >> "$GITHUB_OUTPUT"', builder)
        self.assertIn('affected: ${{ steps.affected.outputs.affected }}', builder)
        self.assertIn('echo affected=true >> "$GITHUB_OUTPUT"', builder)
        self.assertEqual(builder.count('python3 tools/ci/app_sdk_affected.py'), 1)
        self.assertIn('--lane both --github-output "$GITHUB_OUTPUT"', builder)

    def test_pull_requests_and_pushes_share_one_path_list(self) -> None:
        def paths(event: str) -> list[str]:
            body = re.search(rf"^  {event}:\n((?:    .*\n)*)", TRIGGERS, re.M).group(1)
            return re.findall(r'^      - "(.+)"$', body, re.M)

        self.assertIn("crates/**", paths("push"))
        self.assertEqual(paths("pull_request"), paths("push"))

    def test_jobs_test_the_same_main(self) -> None:
        for name in ("policy", "linux", "web", "web-size"):
            body = job(name)
            self.assertEqual(body.count("uses: ./.forgejo/actions/merge-main"), 1, name)
            self.assertIn("main: ${{ needs.builder.outputs.main }}", body, name)

    def test_manual_linux_diagnosis_keeps_normal_browser_gates(self) -> None:
        self.assertIn('default: all', block('on', 0))
        self.assertIn('options: [all, linux]', block('on', 0))
        selector = "(github.event_name != 'workflow_dispatch' || github.event.inputs.lanes != 'linux')"
        for name in ('web', 'web-size'):
            body = job(name)
            self.assertIn(selector + " && !cancelled()", body)
            self.assertIn("needs.builder.outputs.affected == 'true'", body)
            self.assertIn("needs.policy.result == 'success'", body)
        self.assertNotIn(selector, job('linux'))

    def test_macos_lane_lives_on_the_development_mac(self) -> None:
        self.assertNotIn("macos", JOBS)
        self.assertIn("sdk-macos", SOURCE)

    def test_linux_uses_portable_native_prs_and_keeps_main_x86_qualification(self) -> None:
        body = job("linux")
        self.assertIn("runs-on: ${{ github.event_name == 'push' && 'runner' || 'runner-native-portable' }}", body)
        self.assertIn("EXPECTED_ARCH: ${{ github.event_name == 'push' && 'x86_64' || runner.arch }}", body)
        self.assertIn('run: bash tools/ci/assert-native-runner.sh "$EXPECTED_ARCH"', body)
        for name in ("web", "web-size"):
            self.assertIn("    runs-on: runner\n", job(name))


if __name__ == "__main__":
    unittest.main()
