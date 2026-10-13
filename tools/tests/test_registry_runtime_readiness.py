"""Exercise the publisher's bounded exact-runtime readiness wait."""

from pathlib import Path
import shlex
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / 'tools/apps/registry-runtime-readiness.sh'


class RegistryRuntimeReadinessTest(unittest.TestCase):
    def run_case(self, body):
        with tempfile.TemporaryDirectory() as directory:
            script = (
                'set -euo pipefail\n'
                f'cd {shlex.quote(directory)}\n'
                f'source {shlex.quote(str(HELPER))}\n'
                'readiness_deadline=$((SECONDS + 1200))\n'
                + textwrap.dedent(body)
            )
            return subprocess.run(['bash', '-c', script], text=True, capture_output=True)

    def test_delayed_signed_promotion_retries_then_emits_one_entry(self):
        result = self.run_case('''
            attempts=0; fetches=0; promoted=false
            cli() {
              attempts=$((attempts + 1))
              if [[ $promoted == true ]]; then
                printf 'one signed entry\\n'
                return 0
              fi
              printf '%s\\n' '{"package_id":"so.tilo.agent","version":"1.0.8","architecture":"aarch64","runtime":"tilo-runtime/exact"}' > refusal
              printf 'exact runtime is not yet offered\\n' >&2
              return 1
            }
            sleep() { :; }
            fetch_snapshot() { fetches=$((fetches + 1)); promoted=true; }
            await_registry_runtime_ready "$readiness_deadline" request refusal cli
            [[ $attempts == 2 && $fetches == 1 ]]
            [[ $(cat request) == 'one signed entry' ]]
            [[ ! -e refusal ]]
        ''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('waiting for signed-catalogue promotion', result.stderr)

    def test_genuinely_missing_provider_fails_after_bound(self):
        result = self.run_case('''
            attempts=0; fetches=0
            cli() {
              attempts=$((attempts + 1))
              printf '%s\\n' '{"package_id":"so.tilo.agent","version":"1.0.8","architecture":"aarch64","runtime":"tilo-runtime/missing"}' > refusal
              return 1
            }
            sleep() { :; }
            fetch_snapshot() { fetches=$((fetches + 1)); SECONDS=1201; }
            if await_registry_runtime_ready "$readiness_deadline" request refusal cli; then exit 1; fi
            [[ $attempts == 2 && $fetches == 1 ]]
            [[ ! -s request ]]
        ''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('still absent from the signed catalogue after 20 minutes', result.stderr)

    def test_deadline_is_shared_across_multiple_entries(self):
        result = self.run_case('''
            first() { printf 'first signed entry\\n'; }
            await_registry_runtime_ready "$readiness_deadline" first_request refusal first
            SECONDS=$((readiness_deadline + 1))
            fetches=0
            second() {
              printf '%s\\n' '{"package_id":"so.tilo.chat","version":"1.0.9","architecture":"x86_64","runtime":"tilo-runtime/missing"}' > refusal
              return 1
            }
            fetch_snapshot() { fetches=$((fetches + 1)); }
            if await_registry_runtime_ready "$readiness_deadline" second_request refusal second; then exit 1; fi
            [[ $fetches == 0 ]]
            [[ ! -s second_request ]]
        ''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('still absent from the signed catalogue after 20 minutes', result.stderr)

    def test_signature_or_packaging_error_is_not_retried(self):
        result = self.run_case('''
            attempts=0; fetches=0
            cli() { attempts=$((attempts + 1)); printf 'invalid signature\\n' >&2; return 1; }
            fetch_snapshot() { fetches=$((fetches + 1)); }
            if await_registry_runtime_ready "$readiness_deadline" request refusal cli; then exit 1; fi
            [[ $attempts == 1 && $fetches == 0 ]]
        ''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('invalid signature', result.stderr)

    def test_cancelled_wait_stops_before_refetch_or_publication(self):
        result = self.run_case('''
            cli() {
              printf '%s\\n' '{"package_id":"so.tilo.agent","version":"1.0.8","architecture":"aarch64","runtime":"tilo-runtime/exact"}' > refusal
              return 1
            }
            sleep() { return 130; }
            fetch_snapshot() { printf 'unexpected refetch\\n'; }
            await_registry_runtime_ready "$readiness_deadline" request refusal cli
            printf 'unexpected publication\\n'
        ''')
        self.assertEqual(result.returncode, 130, result.stderr)
        self.assertNotIn('unexpected', result.stdout)
        self.assertIn('waiting for signed-catalogue promotion', result.stderr)

    def test_already_ready_entry_does_not_wait_or_refetch(self):
        result = self.run_case('''
            attempts=0; fetches=0
            cli() { attempts=$((attempts + 1)); printf 'unchanged signed entry\\n'; }
            fetch_snapshot() { fetches=$((fetches + 1)); }
            await_registry_runtime_ready "$readiness_deadline" request refusal cli
            [[ $attempts == 1 && $fetches == 0 ]]
            [[ $(cat request) == 'unchanged signed entry' ]]
        ''')
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
