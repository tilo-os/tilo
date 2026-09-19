#!/usr/bin/env python3
"""Offline checks of the retained private Git CAS race and mixed-source receipts."""
import json
from pathlib import Path

path = Path(__file__).parent / 'results/atomic-channel-20260919/evidence.json'
evidence = json.loads(path.read_text())
old = evidence['expected_head']
candidates = evidence['prepared']
assert len(candidates) == 2 and len({p['candidate'] for p in candidates}) == 2
assert all(p['expected_head'] == old and p['manifest']['previous_channel_commit'] == old for p in candidates)
assert sorted(p['accepted'] for p in evidence['pushes']) == [False, True]
assert sorted(o['outcome'] for o in evidence['outcomes']) == ['not-current', 'promoted']
winner = next(o['candidate'] for o in evidence['outcomes'] if o['outcome'] == 'promoted')
assert winner == evidence['final_head']
manifest = next(p['manifest'] for p in candidates if p['candidate'] == winner)
assert manifest == evidence['final_manifest']
assert manifest['architectures']['aarch64'] == evidence['initial_manifest']['architectures']['aarch64']
assert manifest['architectures']['x86_64']['source_sha'] == manifest['source_sha']
assert manifest['architectures']['aarch64']['source_sha'] != manifest['source_sha']
stale = evidence['restart_stale_attempt']
assert stale['generation'] > manifest['generation'] and stale['returncode'] != 0
assert stale['source_sha'] == evidence['initial_manifest']['source_sha']
assert stale['reason'] == 'source ancestry regression'
assert evidence['production_publishing'] is False
print('PASS: one concurrent winner, atomic mixed-source manifest, stale source rejected after restart')
