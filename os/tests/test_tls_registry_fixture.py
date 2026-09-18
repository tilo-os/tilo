import subprocess
import unittest
from unittest.mock import patch

import tls_registry_fixture as fixture


class Response:
    def __init__(self, status):
        self.status = status

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False


class TlsRegistryFixtureTests(unittest.TestCase):
    def test_run_preserves_bounded_stderr(self):
        stderr = ('discarded-prefix-' + 'x' * fixture.DIAGNOSTIC_LIMIT).encode()
        result = subprocess.CompletedProcess([], 125, b'', stderr)
        with patch.object(fixture.subprocess, 'run', return_value=result), self.assertRaisesRegex(
                RuntimeError, rf'^fixture-tool failed with exit 125: x{{{fixture.DIAGNOSTIC_LIMIT}}}$'):
            fixture.run('/usr/bin/fixture-tool')

    def test_readiness_retries_until_the_tls_endpoint_answers(self):
        outcomes = iter((OSError('starting'), OSError('still starting'), Response(200)))

        def open_url(*_args, **_kwargs):
            outcome = next(outcomes)
            if isinstance(outcome, Exception):
                raise outcome
            return outcome

        with patch.object(fixture.time, 'sleep') as sleep:
            fixture.wait_until_ready('127.0.0.1:5000', object(), attempts=3, opener=open_url)
        self.assertEqual(sleep.call_count, 2)

    def test_readiness_failure_reports_bound_and_last_probe(self):
        with patch.object(fixture.time, 'sleep'), self.assertRaisesRegex(
                RuntimeError,
                r'did not become ready after 0\.5s; last probe: OSError: refused'):
            fixture.wait_until_ready(
                '127.0.0.1:5000', object(), attempts=2, delay=0.25,
                opener=lambda *_args, **_kwargs: (_ for _ in ()).throw(OSError('refused')),
            )

    def test_registry_diagnostics_include_state_logs_and_exit_codes(self):
        results = iter((
            subprocess.CompletedProcess([], 0, b'{"Running":false}\n', b''),
            subprocess.CompletedProcess([], 1, b'listen failed\n', b''),
        ))
        with patch.object(fixture.subprocess, 'run', side_effect=lambda *_args, **_kwargs: next(results)):
            detail = fixture.registry_diagnostics('fixture')
        self.assertIn('state (exit 0): {"Running":false}', detail)
        self.assertIn('logs (exit 1): listen failed', detail)


if __name__ == '__main__':
    unittest.main()
