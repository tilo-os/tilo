#!/usr/bin/env python3
"""Race two prepared private fixture promotions; explicit apply is required."""
import argparse
import concurrent.futures
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('channel', ROOT / 'tools/atomic_app_channel.py')
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--git-dir', type=Path, required=True)
    parser.add_argument('--inputs', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--apply-private-fixture', action='store_true')
    args = parser.parse_args()
    git = c.Git(args.git_dir)
    expected = git.head()
    initial_manifest = git.manifest(expected)
    proposals = [json.loads((args.inputs / (side + '.json')).read_text()) for side in ('L', 'R')]
    prepared = [c.prepare(git, expected, p['source_sha'], p['generation'], p['architectures']) for p in proposals]
    if not args.apply_private_fixture:
        print(json.dumps({'dry_run': True, 'prepared': prepared}, indent=2))
        return
    barrier = threading.Barrier(2)

    class RacingGit(c.Git):
        def push(self, candidate):
            barrier.wait(timeout=30)
            try:
                output = super().push(candidate)
                self.push_evidence = {'accepted': True, 'output': output}
                return output
            except Exception as error:
                self.push_evidence = {'accepted': False, 'output': str(error)}
                raise

    workers = [RacingGit(args.git_dir) for _ in prepared]
    # Prevalidate/fetch serially so the shared local bare repository is not asked
    # to update FETCH_HEAD concurrently. The two remote ref writes still race.
    for worker, proposal in zip(workers, proposals):
        c.assemble(worker, expected, proposal['source_sha'], proposal['generation'], proposal['architectures'])
    with concurrent.futures.ThreadPoolExecutor(2) as pool:
        futures = [pool.submit(c.promote, worker, plan['candidate'], expected) for worker, plan in zip(workers, prepared)]
        outcomes = [future.result() for future in futures]
    current = git.head()
    manifest = git.manifest(current)
    c.check(sorted(x['outcome'] for x in outcomes) == ['not-current', 'promoted'], 'expected exactly one winner')
    c.check(manifest['architectures']['aarch64'] == initial_manifest['architectures']['aarch64'], 'partial update changed ARM identity')
    stale = json.loads((args.inputs / 'initial.json').read_text())
    stale['generation'] = 1000
    stale_path = args.inputs / 'stale-after-restart.json'
    stale_path.write_text(json.dumps(stale))
    restarted = subprocess.run([sys.executable, str(ROOT / 'tools/atomic_app_channel.py'), '--git-dir', str(args.git_dir),
                                '--expected-head', current, '--promotion', str(stale_path), '--apply-private-fixture'],
                               capture_output=True, text=True, timeout=120)
    c.check(restarted.returncode != 0 and 'source ancestry regression' in restarted.stderr, 'stale source was not rejected after restart')
    c.check(git.head() == current, 'stale attempt advanced channel')
    evidence = {'repository': c.REPO, 'ref': c.REF, 'expected_head': expected, 'prepared': prepared,
                'outcomes': outcomes, 'pushes': [w.push_evidence for w in workers], 'final_head': current,
                'initial_manifest': initial_manifest, 'final_manifest': manifest,
                'restart_stale_attempt': {'source_sha': stale['source_sha'], 'generation': 1000,
                                           'returncode': restarted.returncode, 'reason': 'source ancestry regression'},
                'production_publishing': False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(c.encode(evidence))
    print(json.dumps({'outcomes': outcomes, 'stale_source_rejected_after_restart': True}))


if __name__ == '__main__':
    main()
