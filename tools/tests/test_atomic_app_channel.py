import concurrent.futures
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

SPEC = importlib.util.spec_from_file_location('channel', Path(__file__).parents[1] / 'atomic_app_channel.py')
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)


class LocalGit(c.Git):
    """Same Git operations against a real temporary bare server; no network."""
    def __init__(self, directory, server):
        self.server = str(server)
        self.barrier = None
        super().__init__(directory)

    def call(self, *args, data=None):
        if args[0] in ('fetch', 'push', 'ls-remote'):
            args = tuple(self.server if x == 'origin' else x for x in args)
        return super().call(*args, data=data)

    def qualify(self):
        pass

    def push(self, candidate):
        if self.barrier:
            self.barrier.wait(timeout=10)
        return super().push(candidate)


class ChannelTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.client, self.server = root / 'client.git', root / 'server.git'
        for path in (self.client, self.server):
            subprocess.run(['git', 'init', '--bare', str(path)], capture_output=True, check=True)
        subprocess.run(['git', '--git-dir=' + str(self.client), 'remote', 'add', 'origin', c.REMOTE], check=True)
        self.git = LocalGit(self.client, self.server)
        self.a = self.commit_files(None, {'source.txt': b'A'})
        self.git.call('push', 'origin', self.a + ':' + c.REF)
        self.initial = self.artifacts(self.a, 1, ('x86_64', 'aarch64'))
        plan = c.prepare(self.git, self.a, self.a, 1, self.initial)
        self.assertEqual(c.promote(self.git, plan['candidate'], self.a)['outcome'], 'promoted')
        self.old = plan['candidate']
        self.b = self.commit_files(self.a, {'source.txt': b'B'})
        self.updates = self.artifacts(self.b, 2, ('x86_64',))

    def tearDown(self):
        self.temp.cleanup()

    def commit_files(self, parent, files):
        # Build nested fixture trees with a temporary index, leaving normal refs alone.
        index = Path(self.temp.name) / ('index-' + str(len(list(Path(self.temp.name).glob('index-*')))))
        import os
        env = dict(os.environ, GIT_INDEX_FILE=str(index))
        base = ['git', '--git-dir=' + str(self.client)]
        subprocess.run(base + ['read-tree', '--empty'], env=env, capture_output=True, check=True)
        for name, content in files.items():
            blob = self.git.call('hash-object', '-w', '--stdin', data=content).decode().strip()
            subprocess.run(base + ['update-index', '--add', '--cacheinfo', '100644', blob, name], env=env, capture_output=True, check=True)
        tree = subprocess.check_output(base + ['write-tree'], env=env).decode().strip()
        args = ['-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid', 'commit-tree', tree]
        if parent:
            args += ['-p', parent]
        return self.git.call(*args, data=b'fixture\n').decode().strip()

    def artifacts(self, source, generation, arches):
        files, payloads = {}, {}
        for arch in arches:
            runtime = {'version': str(generation), 'root_hash': str(generation) * 64, 'publisher': '1' * 64}
            payloads[arch] = {'kind': 'nonpublishing-channel-fixture', 'architecture': arch, 'source_sha': source, 'runtime': runtime}
            files['fixture-artifacts/' + arch + '.json'] = c.encode(payloads[arch])
        commit = self.commit_files(source, files)
        self.git.call('push', 'origin', commit + ':refs/heads/artifacts-' + commit)
        return {arch: {'architecture': arch, 'source_sha': source, 'runtime': payloads[arch]['runtime'],
                       'artifact_commit': commit, 'sha256': hashlib.sha256(files['fixture-artifacts/' + arch + '.json']).hexdigest(),
                       'url': f'https://code.podesta.ai/api/v1/repos/{c.REPO}/raw/fixture-artifacts/{arch}.json?ref={commit}'}
                for arch in arches}

    def test_actual_nonforce_race_one_winner_and_partial_preservation(self):
        left = c.prepare(self.git, self.old, self.b, 2, self.updates)
        alternate = self.artifacts(self.b, 3, ('x86_64',))
        right = c.prepare(self.git, self.old, self.b, 2, alternate)
        workers = [LocalGit(self.client, self.server), LocalGit(self.client, self.server)]
        barrier = threading.Barrier(2)
        for worker in workers:
            worker.barrier = barrier
        # Fetch once before racing to avoid concurrent FETCH_HEAD noise. The
        # independently validated candidate pushes still race at the real server.
        for worker, plan in zip(workers, (left, right)):
            c.assemble(worker, self.old, self.b, 2, {k: v for k, v in plan['manifest']['architectures'].items() if k == 'x86_64'})
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            futures = [pool.submit(c.promote, worker, plan['candidate'], self.old) for worker, plan in zip(workers, (left, right))]
            outcomes = [f.result()['outcome'] for f in futures]
        self.assertEqual(sorted(outcomes), ['not-current', 'promoted'])
        current = self.git.manifest(self.git.head())
        self.assertEqual(current['architectures']['aarch64'], self.initial['aarch64'])
        self.assertEqual(current['architectures']['x86_64']['source_sha'], self.b)

    def test_higher_generation_cannot_regress_source_after_restart(self):
        plan = c.prepare(self.git, self.old, self.b, 2, self.updates)
        c.promote(self.git, plan['candidate'], self.old)
        restarted = LocalGit(self.client, self.server)
        with self.assertRaisesRegex(ValueError, 'source ancestry regression'):
            c.prepare(restarted, plan['candidate'], self.a, 1000, self.initial)

    def test_bad_artifact_digest_blocks_channel_advance(self):
        receipt = dict(self.updates['x86_64'], sha256='0' * 64)
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            c.prepare(self.git, self.old, self.b, 2, {'x86_64': receipt})
        self.assertEqual(self.git.head(), self.old)

    def test_lost_push_response_reconciles_without_retry(self):
        plan = c.prepare(self.git, self.old, self.b, 2, self.updates)
        push, calls = self.git.push, []
        def lose_response(candidate):
            calls.append(candidate)
            push(candidate)
            raise TimeoutError()
        self.git.push = lose_response
        self.assertEqual(c.promote(self.git, plan['candidate'], self.old)['outcome'], 'promoted')
        self.assertEqual(calls, [plan['candidate']])

    def test_product_pushurl_rejected(self):
        self.git.call('remote', 'set-url', '--push', 'origin', 'ssh://git@code.podesta.ai/qos/qos.git')
        with self.assertRaisesRegex(ValueError, 'private fixture remote'):
            LocalGit(self.client, self.server)


if __name__ == '__main__':
    unittest.main()
