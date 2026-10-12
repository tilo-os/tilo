"""Coverage boundaries of the production feedback and qualification workflows."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).parents[2]


class FeedbackScopeTests(unittest.TestCase):
    def test_feedback_and_nightly_scopes_cover_the_intended_tests(self):
        with self.subTest(phase='feedback checks closure but tests explicit test scope'):
            workflow = (ROOT / '.forgejo/workflows/check.yml').read_text(encoding='utf-8')
            self.assertRegex(workflow, r'python3 tools/ci/compiler_inputs\.py --output target/ci/compiler-inputs\.json --\s+check --locked --all-targets \$\{\{ matrix\.packages \}\}')
            self.assertIn('cargo nextest run --locked --profile ci --tests ${{ matrix.test_packages }}', workflow)
            self.assertIn("if: matrix.test_packages != ''", workflow)
            self.assertIn("if: contains(matrix.test_packages, '-p app-cli')", workflow)
            # A shard's affected apps pass the nightly's native certification.
            self.assertIn('certify --workspace ${{ matrix.test_packages }}', workflow)
            self.assertIn('needs: [builder, select, security, test, check, clippy, android, wasm, suites, pins, tapps]', workflow)
            self.assertIn('test "$BUILDER_RESULT" = success', workflow)
            self.assertIn('*:true:success)', workflow)
            self.assertIn('pull_request:false:skipped)', workflow)
            self.assertIn("if: needs.select.outputs.security_required == 'true'", workflow)
            # Manual acceptance forces full native qualification; ordinary
            # before-SHA handling does not. Temporary packaging/ARM branch
            # full-check triggers were removed — use workflow_dispatch.
            self.assertIn('run: bash tools/ci/select.sh', workflow)
            script = (ROOT / 'tools/ci/select.sh').read_text(encoding='utf-8')
            selection = script.split('selection_args=()', 1)[1].split('elif [ -z "$base" ]', 1)[0]
            self.assertEqual(selection.count('selection_args+=(--full)'), 1)
            self.assertIn('workflow_dispatch', selection)
            self.assertNotIn('package-desktop-release', selection)
            self.assertNotIn('arm64-platform-support', workflow)
        with self.subTest(phase='nightly executes complete coverage on both native architectures'):
            workflow = (ROOT / '.forgejo/workflows/ci-nightly.yml').read_text(encoding='utf-8')
            pairs = re.findall(r'- arch: (\S+)\s+runner: (\S+)', workflow)
            self.assertEqual(pairs, [('x86_64', 'runner-disk-100g'), ('aarch64', 'runner-disk-100g-arm64')])
            self.assertIn('runs-on: ${{ matrix.runner }}', workflow)
            self.assertIn("./os/dev/assert-native-arch.sh '${{ matrix.arch }}'", workflow)
            self.assertIn('lvp_icd.${{ matrix.arch }}.json', workflow)
            self.assertIn('cargo nextest run --workspace --locked --profile ci', workflow)
            self.assertIn('cargo check --workspace --locked --all-targets', workflow)
            self.assertIn('cargo test --workspace --examples --locked', workflow)
            self.assertIn('fail-fast: false', workflow)
            self.assertNotIn("if: matrix.arch", workflow)
