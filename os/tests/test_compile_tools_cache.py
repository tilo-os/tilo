import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('tools', Path(__file__).parents[1] / 'dev/build-compile-tools.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
BASE = 'code.podesta.ai/qos/tilo-builder@sha256:' + 'a' * 64

class CacheTests(unittest.TestCase):
    def test_all_effective_inputs_change_key(self):
        values = [b'recipe', BASE, b'cosign', 'linux/arm64']
        expected = m.fingerprint(*values)
        for index, value in enumerate([b'new recipe', BASE[:-1] + 'b', b'new cosign', 'linux/amd64']):
            changed = values.copy(); changed[index] = value
            self.assertNotEqual(expected, m.fingerprint(*changed))

    def test_optional_timeout_and_failure_are_misses(self):
        for error in [subprocess.TimeoutExpired('docker', 1), subprocess.CalledProcessError(1, 'docker'), OSError('missing')]:
            with patch.object(m.subprocess, 'run', side_effect=error):
                self.assertIsNone(m.optional(['docker', 'pull', 'fixture']))

    def execute(self, cached=False, failure=False):
        with tempfile.TemporaryDirectory() as path:
            root = Path(path)
            (root / 'Containerfile').write_bytes(b'FROM fixture\nCOPY cosign /cosign\n')
            (root / 'cosign').write_bytes(b'fixture')
            def optional(argv, **kwargs):
                if argv[1] == 'pull':
                    return subprocess.CompletedProcess(argv, 0) if cached else None
                if argv[1] == 'image':
                    return subprocess.CompletedProcess(argv, 0, ('["' + m.REPOSITORY + '@sha256:' + 'f'*64 + '"]').encode())
                return None  # Publication failure must not fail completed build.
            with patch.object(m, 'optional', side_effect=optional), patch.object(m.subprocess, 'run') as run, patch.object(m.subprocess, 'check_output', return_value=b'linux/arm64\n'), redirect_stdout(io.StringIO()):
                if failure: run.side_effect = subprocess.CalledProcessError(23, 'docker')
                if failure:
                    with self.assertRaises(subprocess.CalledProcessError):m.build(root, BASE, 'linux/arm64', 'fixture:tools')
                else:m.build(root, BASE, 'linux/arm64', 'fixture:tools')
                args = run.call_args.args[0]
                self.assertEqual('--cache-from' in args, cached)
                if cached:self.assertIn(m.REPOSITORY + '@sha256:' + 'f'*64, args)
                self.assertIn('BUILDKIT_INLINE_CACHE=1', args)

    def test_cache_miss_still_builds(self): self.execute()
    def test_cache_hit_uses_digest_but_still_builds(self): self.execute(cached=True)
    def test_authoritative_build_failure_propagates(self): self.execute(failure=True)

if __name__ == '__main__': unittest.main()
