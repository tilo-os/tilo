#!/usr/bin/env python3
"""Resume frozen4583 native payload phases; requires explicit reconstruction adoption.

Run in an admitted native overlay CI worker with the original compiler-cache
environment. This is not a workflow dispatcher or a partial-receipt converter.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import time

import native_resume as resume


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def build_arguments(value):
    tokens = shlex.split(value)
    values = {}
    allowed = {'TILO_PREBUILT_APPS', 'TILO_PREBUILT_DESKTOP', 'TILO_CARGO_BUILD_JOBS',
               'TILO_REQUIRE_COMPILER_CACHE', 'SCCACHE_ENDPOINT', 'SCCACHE_BUCKET',
               'SCCACHE_REGION', 'SCCACHE_S3_USE_SSL', 'SCCACHE_S3_KEY_PREFIX'}
    resume.require(len(tokens) % 2 == 0, 'invalid original build arguments')
    for flag, value in zip(tokens[::2], tokens[1::2]):
        resume.require(flag in ('--build-arg', '--secret'), 'unsupported build override')
        if flag == '--build-arg':
            key, separator, content = value.partition('=')
            resume.require(separator and key in allowed and key not in values, 'unsupported or repeated build argument')
            values[key] = content
            if key.startswith('TILO_PREBUILT_'):
                resume.require(content == '0', 'prebuilt graph differs')
            if key == 'TILO_CARGO_BUILD_JOBS':
                resume.require(content == '2', 'original compiler jobs differ')
            if key == 'TILO_REQUIRE_COMPILER_CACHE':
                resume.require(content == '1', 'normal compiler cache required')
        else:
            resume.require(re.fullmatch(r'id=aws_(access_key_id|secret_access_key|session_token),src=/[^\s,]+', value),
                           'unexpected cache secret')
    expected = {'TILO_PREBUILT_APPS': '0', 'TILO_PREBUILT_DESKTOP': '0', 'TILO_CARGO_BUILD_JOBS': '2',
                'TILO_REQUIRE_COMPILER_CACHE': '1', 'SCCACHE_ENDPOINT': 'https://s3.us-west-2.amazonaws.com',
                'SCCACHE_BUCKET': 'tilo-ci-sccache-125650147312-us-west-2',
                'SCCACHE_REGION': 'us-west-2', 'SCCACHE_S3_USE_SSL': 'true'}
    resume.require(all(values.get(k) == v for k,v in expected.items()), 'original build/cache configuration required')
    prefix = values.get('SCCACHE_S3_KEY_PREFIX', '')
    resume.require(len(prefix) <= 128 and re.fullmatch(r'[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*', prefix), 'canonical cache namespace required')


def build_bounded(script, environment, timeout):
    process = subprocess.Popen(['/bin/bash', str(script)], env=environment, start_new_session=True)
    deadline = time.monotonic() + timeout
    try:
        while process.poll() is None:
            resume.require(min(shutil.disk_usage(p).free for p in ('/work', '/var/lib/containers')) >= 8 * 1024**3,
                           'reconstruction reached scratch reserve')
            resume.require(time.monotonic() < deadline, 'reconstruction time bound exceeded')
            time.sleep(5)
        resume.require(process.returncode == 0, 'resumed native build failed')
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--source', type=Path, required=True)
    p.add_argument('--inputs', type=Path, required=True)
    p.add_argument('--environment', type=Path, required=True)
    p.add_argument('--checkpoint', nargs=2, action='append', required=True, metavar=('RECEIPT', 'SHA256'))
    p.add_argument('--reconstructed-partial-admission', nargs=2, metavar=('JSON','SHA256'))
    p.add_argument('--evidence', type=Path, required=True)
    p.add_argument('--run-id', required=True)
    p.add_argument('--attempt', required=True)
    p.add_argument('--timeout-seconds', type=int, default=9000)
    p.add_argument('--thin-cache-approval', nargs=2, metavar=('APPROVAL', 'SHA256'))
    args = p.parse_args()
    resume.require(1 <= len(args.checkpoint) <= 4 and 60 <= args.timeout_seconds <= 9000, 'bounded reconstruction required')
    resume.require(args.run_id.isdecimal() and args.attempt.isdecimal(), 'numeric reconstruction run required')
    source = args.source.resolve()
    revision = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD']).decode().strip()
    resume.require(revision == resume.SOURCE, 'frozen source4583 required')
    resume.require(not subprocess.check_output(['git', '-C', str(source), 'status', '--porcelain', '--untracked-files=all']).strip(),
                   'frozen source checkout must be clean before loading helpers')
    native = resume.load_native(source)
    t = native.tools
    inputs = t.strict_json(t.read_bounded(args.inputs, 65536))
    expected = native.expected_inputs(source, resume.SOURCE, 'aarch64', inputs['base'], inputs['rpms'],
        inputs['wpe_reference'], inputs['wpe_receipt_sha256'], inputs['epoch'])
    resume.require(inputs == expected, 'original native input contract differs')
    base_pin = re.search(r'^BASE_REF=(\S+)', (source / 'os/base-snapshot.env').read_text(), re.M)
    resume.require(base_pin and base_pin[1].strip('\"\'') == inputs['base'], 'frozen base pin differs')
    env = t.strict_json(t.read_bounded(args.environment, 65536))
    resume.require(isinstance(env, dict) and all(isinstance(k, str) and isinstance(v, str) for k,v in env.items()), 'string environment required')
    for name in ('TILO_NATIVE_BUILDER_IMAGE_ID', 'TILO_NATIVE_BUILDER_RECEIPT', 'CONTAINER_HOST', 'DOCKER_HOST', 'PODMAN_HOST'):
        resume.require(not env.get(name), 'unexpected native/remote override')
    resume.require(env.get('TILO_WITH_BROWSER') == '1' and env.get('TILO_PUBLIC_TRUST_MODE') == 'development'
                   and env.get('TILO_SKIP_BUILD') == '0', 'original source build mode required')
    resume.require(env.get('RPMS_REF') == inputs['rpms'] and env.get('SOURCE_DATE_EPOCH') == str(inputs['epoch'])
                   and env.get('TILO_WPE_ADOPTION_SHA256') == inputs['wpe_receipt_sha256'], 'environment provenance differs')
    build_arguments(env.get('TILO_BUILD_ARGS', ''))
    t.native_host('aarch64')
    resume.require(json.loads(t.command('podman', '--remote=false', 'info', '--format=json'))['store']['graphDriverName'] == 'overlay', 'native overlay required')
    resume.require(min(shutil.disk_usage(p).free for p in ('/work', '/var/lib/containers')) >= 80 * 1024**3,
                   'original 80GiB initial scratch admission required')
    resume.require(not args.evidence.exists(), 'fresh evidence directory required; no automatic replay')
    args.evidence.mkdir(mode=0o700, parents=True)
    builder = 'registry.tilo.so/tilo/tilo-builder@sha256:568fca1f8b4a4b1700c871cca033d2d291a5eb02c4b3be9681410f456b5a7bc9'
    with tempfile.TemporaryDirectory(prefix='tilo-native-resume-', dir='/work') as temporary:
        work = Path(temporary)
        partial = work / 'partial'
        partial.mkdir()
        union, checkpoints = {}, []
        approval = None
        approval_sha = None
        if args.reconstructed_partial_admission:
            path, approval_sha = args.reconstructed_partial_admission
            raw = t.read_bounded(Path(path), 1024 * 1024)
            resume.require(re.fullmatch('[0-9a-f]{64}', approval_sha) and t.sha(raw) == approval_sha,
                           'independent reconstructed partial admission hash required')
            approval = t.strict_json(raw)
        for path, sha in args.checkpoint:
            raw = t.read_bounded(Path(path), 1024 * 1024)
            resume.require(re.fullmatch('[0-9a-f]{64}', sha) and t.sha(raw) == sha, 'independent checkpoint hash required')
            checkpoints.append({'receipt_sha256': sha, 'receipt': t.strict_json(raw)})
        legacy = [c for c in checkpoints if c['receipt']['purpose'] == resume.PARTIAL]
        newer = [c for c in checkpoints if c['receipt']['purpose'] != resume.PARTIAL]
        for checkpoint in legacy:
            resume.validate_partial(native, checkpoint['receipt'], inputs, builder, partial, union)
        legacy_union = dict(union)
        resume.require(not newer or approval is not None, 'reconstructed partial requires explicit admission')
        if approval is not None:
            resume.require({c['receipt_sha256'] for c in newer} == set(approval['receipts']),
                           'admission must match exactly the supplied reconstructed partials')
        for checkpoint in newer:
            resume.validate_reconstructed_partial(native, checkpoint['receipt'], checkpoint['receipt_sha256'],
                approval, source, legacy, legacy_union, partial, union)
        original = (source / 'os/Containerfile').read_text()
        thin_context, thin_provenance = None, None
        if args.thin_cache_approval:
            import thin_cache_stage
            thin_context, thin_provenance = thin_cache_stage.stage(native, *args.thin_cache_approval, work)
        derived, missing = resume.derive_recipe(original, union, native.REQUIRED,
            thin_cache_approval_sha256=args.thin_cache_approval[1] if args.thin_cache_approval else None)
        recipe = work / 'Containerfile.resumed'
        recipe.write_text(derived)
        seed_recipe = work / 'Containerfile.seed'
        seed_recipe.write_text('FROM scratch\nCOPY partial/ /partial/\n')
        seed_iid = work / 'seed.iid'
        t.command('podman', '--remote=false', 'build', '--network=none', '--pull=never', '--file', str(seed_recipe),
                  '--iidfile', str(seed_iid), str(work))
        seed = seed_iid.read_text().strip()
        t.inspect_local(seed, 'aarch64', seed)
        original_driver = (source / 'os/dev/build-image.sh').read_text()
        driver = resume.derive_driver(original_driver, str(source), str(recipe), seed)
        script = work / 'build-resumed.sh'
        script.write_text(driver)
        reconstruction = {'schema': 1, 'purpose': 'tilo-native-reconstruction', 'source_revision': resume.SOURCE,
            'original_recipe_sha256': digest(source / 'os/Containerfile'), 'derived_recipe_sha256': digest(recipe),
            'original_driver_sha256': digest(source / 'os/dev/build-image.sh'), 'derived_driver_sha256': digest(script),
            'source_path': str(source), 'derived_recipe_path': str(recipe), 'seed_image_id': seed,
            'resume_helper_sha256': digest(Path(__file__).with_name('native_resume.py')),
            'producer_sha256': digest(__file__), 'checkpoints': checkpoints, 'reused_inventory': union,
            'missing_targets': missing, 'run_id': args.run_id, 'attempt': args.attempt,
            'remaining_phases': ['thin-app-shared-graph', 'elf-hardening', 'runtime-bundle', 'app-store'],
            'original_recipe_executed_verbatim': False}
        if approval_sha is not None:
            reconstruction['reconstructed_partial_admission_sha256'] = approval_sha
            reconstruction['reconstructed_partial_admission'] = approval
        if thin_provenance is not None:
            reconstruction['thin_cache'] = thin_provenance
        (args.evidence / 'reconstruction.json').write_bytes(t.canonical(reconstruction))
        (args.evidence / 'Containerfile.resumed').write_text(derived)
        iid = work / 'complete.iid'
        env.update(TILO_NATIVE_BUILDER_ONLY='1', TILO_NATIVE_BUILDER_IIDFILE=str(iid), TILO_DISTROBOX='')
        if thin_context is not None:
            env['TILO_BUILD_ARGS'] += ' --build-context ' + shlex.quote('tilo-thin-cache=' + str(thin_context))
        build_bounded(script, env, args.timeout_seconds)
        identity = iid.read_text().strip()
        with resume.checkpointed_payload(native, identity, inputs, reconstruction, work, args.evidence,
                                         args.run_id, args.attempt) as (root, inventory):
            if thin_provenance is not None:
                runtime_raw, runtime = resume.read_thin_cache_evidence(t, root, args.thin_cache_approval[1])
                reconstruction['thin_cache']['runtime_evidence'] = runtime
                reconstruction['thin_cache']['runtime_evidence_sha256'] = t.sha(runtime_raw)
                (args.evidence / 'thin-cache-restoration.json').write_bytes(runtime_raw)
            for name, record in union.items():
                location = 'out/optional/browser' if name == 'browser' else 'out/' + name
                resume.require(all(inventory[location][key] == value for key,value in record.items()), 'retained executable was replaced')
        reconstruction_sha = t.sha(t.canonical(reconstruction))
        (args.evidence / 'reconstruction.json').write_bytes(t.canonical(reconstruction))
        metadata = t.canonical({'schema': 1, 'purpose': resume.PURPOSE, 'inputs': inputs,
                                'inventory': inventory, 'reconstruction': reconstruction})
        resume.require(len(metadata) <= 16 * 1024 * 1024, 'complete metadata exceeds bound')
        (work / 'metadata.json').write_bytes(metadata)
        projection = work / 'Containerfile.complete'
        projection.write_text('FROM scratch\nCOPY --from=payload /out/ /out/\n'
            'COPY --from=payload /usr/lib/tilo-runtime/ /usr/lib/tilo-runtime/\n'
            'COPY --from=payload /usr/share/tilo-apps/ /usr/share/tilo-apps/\n'
            f'COPY metadata.json /{native.METADATA}\n')
        projected_iid = work / 'projected.iid'
        t.command('podman', '--remote=false', 'build', '--network=none', '--pull=never', '--file', str(projection),
                  '--source-date-epoch', str(inputs['epoch']), '--rewrite-timestamp', '--build-context',
                  'payload=container-image://' + identity, '--iidfile', str(projected_iid), str(work))
        projected = projected_iid.read_text().strip()
        resume.check_complete(native, projected, inputs, t.sha(metadata), reconstruction_sha)
        digest_file = work / 'manifest.digest'
        tag = resume.REPOSITORY + ':candidate-' + args.run_id + '-' + args.attempt
        t.command('podman', '--remote=false', 'push', '--tls-verify=true', '--digestfile', str(digest_file), projected, 'docker://' + tag)
        reference = resume.REPOSITORY + '@' + digest_file.read_text().strip()
        resume.require(t.registry_identity(reference, 'aarch64') == projected, 'published reconstruction differs')
        receipt = {'schema': 1, 'purpose': resume.PURPOSE, 'inputs': inputs, 'reference': reference,
            'config_digest': projected, 'metadata_sha256': t.sha(metadata), 'reconstruction': reconstruction,
            'reconstruction_sha256': reconstruction_sha, 'complete_native_builder': True,
            'production_signed': False, 'installable': False, 'consumer_adoption_required': True}
        (args.evidence / 'reconstructed-native.json').write_bytes(t.canonical(receipt))
        print(json.dumps({'reference': reference, 'reconstruction_sha256': reconstruction_sha,
                          'complete_native_builder': True, 'consumer_adoption_required': True}))


if __name__ == '__main__':
    main()
