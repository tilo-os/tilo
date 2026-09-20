#!/usr/bin/env python3
"""Packaging identity and validation contract shared by workflow and controller.

This module owns no state, dispatch, API client or process scheduling. Pin its
source with the workflow/artifact policy; pin controller implementation separately.
"""
import hashlib
import json
from pathlib import Path
import re
import tomllib

from ci.resolve_builder import resolve


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'))



def receipt_key(receipt):
    return hashlib.sha256(canonical(receipt).encode()).hexdigest()



def require(value, message):
    if not value:
        raise ValueError(message)



def validate_cargo_inputs(toolchain_bytes, config_bytes):
    """Constrain the reviewed Cargo/Rustup inputs used by metadata selection."""
    toolchain = tomllib.loads(toolchain_bytes.decode('utf-8')).get('toolchain', {})
    require(isinstance(toolchain, dict) and 'path' not in toolchain and
            isinstance(toolchain.get('channel'), str) and
            re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', toolchain['channel']),
            'metadata requires a pinned release toolchain, never a candidate path')
    config = tomllib.loads(config_bytes.decode('utf-8'))

    def no_includes(value):
        if isinstance(value, dict):
            require('include' not in value and 'config-include' not in value,
                    'Cargo configuration includes are outside trusted input closure')
            for item in value.values():
                no_includes(item)
        elif isinstance(value, list):
            for item in value:
                no_includes(item)
    no_includes(config)



def validate_local_cargo_inputs(root):
    root = Path(root)
    for override in ('rust-toolchain', '.cargo/config'):
        require(not (root / override).exists() and not (root / override).is_symlink(),
                'untrusted Cargo/Rustup precedence override: ' + override)
    require(not (root / '.cargo').is_symlink(), 'Cargo directory must not be a symlink')
    for path in ('rust-toolchain.toml', '.cargo/config.toml'):
        require((root / path).is_file() and not (root / path).is_symlink(),
                'Cargo policy requires regular source files: ' + path)
    validate_cargo_inputs((root / 'rust-toolchain.toml').read_bytes(),
                          (root / '.cargo/config.toml').read_bytes())



def digest(value, size=64):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(size) + '}', value), 'invalid digest')
    return value



def validate_desired(receipt):
    require(isinstance(receipt, dict) and set(receipt) == {
        'source_sha', 'builder_image', 'collection', 'architectures', 'policy_sha256'}, 'invalid desired identity')
    digest(receipt['source_sha'], 40)
    require(bool(receipt['builder_image']), 'explicit immutable builder required')
    resolve(receipt['builder_image'])
    digest(receipt['policy_sha256'])
    require(re.fullmatch('[a-z][a-z0-9-]*', receipt['collection']), 'invalid collection')
    require(receipt['architectures'] in [['x86_64'], ['aarch64'], ['x86_64', 'aarch64']], 'invalid architecture scope')
    return receipt



def validate_evidence(proof, desired, run_id, final_policy_sha=None):
    require(isinstance(proof, dict) and proof.get('schema') == 'tilo.coordinator-verification/v1', 'invalid verification proof')
    require(type(run_id) is int and run_id > 0 and proof.get('run_id') == run_id, 'verification run mismatch')
    require(proof.get('desired') == desired, 'verification source/builder/scope mismatch')
    require(proof.get('backend') == 'canonical-final-artifact-v1', 'canonical artifact replay required')
    receipts = proof.get('receipts')
    require(isinstance(receipts, dict) and set(receipts) == set(desired['architectures']), 'incomplete architecture verification')
    for value in receipts.values():
        digest(value)
    if final_policy_sha is not None:
        require(proof.get('final_policy_sha256') == digest(final_policy_sha),
                'final verification policy mismatch')
    return proof
