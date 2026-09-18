"""Explicit development reconstruction of complete native payloads from partials.

The old native-builder purpose is deliberately not reused. Consumers require an
independently pinned reconstruction hash and still verify the complete inventory.
"""
from contextlib import contextmanager
import hashlib
import importlib.util
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import shlex
import struct

SOURCE = '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc'
PURPOSE = 'tilo-development-reconstructed-native-builder'
PARTIAL = 'tilo-development-partial-native-checkpoint'
REPOSITORY = 'code.podesta.ai/qos/tilo-development-reconstructed-native-aarch64'
WINDOWS = {'tilo-windows-vm-control', 'tilo-windows-rail'}
THIN_CACHE_EVIDENCE = 'out/thin-cache-restoration.json'


def load_native(repo):
    spec = importlib.util.spec_from_file_location('resume_original_native', Path(repo) / 'os/dev/native_builder_artifact.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require(ok, message):
    if not ok:
        raise ValueError(message)


def replace_once(text, before, after):
    require(text.count(before) == 1, 'frozen recipe seam changed')
    return text.replace(before, after, 1)


def derive_recipe(original, retained, required, *, thin_cache_approval_sha256=None):
    """Keep original package/features graph; omit only completed binary targets."""
    allowed = set(required) | {'browser'}
    require(set(retained) <= allowed and retained, 'invalid checkpoint targets')
    missing = allowed - set(retained)
    release = sorted(missing - WINDOWS)
    windows = sorted(missing & WINDOWS)
    text = original
    marker = '    && cargo clean --profile thin-app \\\n'
    text = replace_once(text, marker, marker + '    && mkdir -p target/release \\\n    && cp /tilo-resume/partial/* target/release/ \\\n')
    # A readonly named image context; no executable helper from the checkpoint.
    start = text.index('RUN --mount=type=cache,target=/build/.cargo-home')
    text = text[:start] + text[start:].replace('RUN --mount=type=cache,target=/build/.cargo-home',
        'RUN --mount=type=bind,from=tilo-resume,target=/tilo-resume,ro \\\n    --mount=type=cache,target=/build/.cargo-home', 1)
    if thin_cache_approval_sha256 is not None:
        require(re.fullmatch('[0-9a-f]{64}', thin_cache_approval_sha256), 'independent thin-cache approval required')
        text = replace_once(text, 'RUN --mount=type=bind,from=tilo-resume,target=/tilo-resume,ro',
            'RUN --mount=type=bind,from=tilo-thin-cache,target=/tilo-thin-cache,ro \\\n    --mount=type=bind,from=tilo-resume,target=/tilo-resume,ro')
        seam = '    && cp /tilo-resume/partial/* target/release/ \\\n'
        text = replace_once(text, seam, seam +
            '    && python3 /tilo-thin-cache/restore-thin-cache.py --approval-sha256 ' + thin_cache_approval_sha256 + ' \\\n')
    before = '       cargo build --release ${RELEASE_ARGS} -p tilo-relay --bins ${BROWSER_PKG}'
    command = ('cargo build --release ${RELEASE_ARGS} -p tilo-relay ' +
               ' '.join('--bin ' + name for name in release) + ' ${BROWSER_PKG}') if release else 'true'
    text = replace_once(text, before, '       ' + command)
    before = '       cargo build --release -p windows-vm \\\n         --bin tilo-windows-vm-control --bin tilo-windows-rail'
    command = 'cargo build --release -p windows-vm ' + ' '.join('--bin ' + name for name in windows) if windows else 'true'
    text = replace_once(text, before, '       ' + command)
    if thin_cache_approval_sha256 is not None:
        # The selected target is a scratch payload, not the builder filesystem.
        # Carry only the public restoration receipt across that stage boundary.
        payload = ('FROM scratch AS native-builder-payload\n'
                   'COPY --from=builder /out/ /out/\n'
                   'COPY --from=builder /usr/lib/tilo-runtime/ /usr/lib/tilo-runtime/\n'
                   'COPY --from=builder /usr/share/tilo-apps/ /usr/share/tilo-apps/\n')
        text = replace_once(text, payload, payload +
            f'COPY --from=builder /build/thin-cache-restoration.json /{THIN_CACHE_EVIDENCE}\n')
    return text, {'release': release, 'windows': windows}


def read_thin_cache_evidence(tools, root, approval_sha256):
    raw = tools.read_bounded(root / THIN_CACHE_EVIDENCE, 65536)
    runtime = tools.strict_json(raw)
    require(runtime['purpose'] == 'tilo-incomplete-thin-cache-restored' and
            runtime['approval_sha256'] == approval_sha256 and
            runtime['cargo_revalidation_required'] is True and runtime['complete'] is False,
            'actual thin cache restoration evidence differs')
    return raw, runtime


@contextmanager
def checkpointed_payload(native, identity, inputs, reconstruction, work, evidence, run_id, attempt):
    '''Retain audited public outputs before fallible receipt validation.

    This purpose is rejected by the complete-artifact importer. A later consumer
    must independently validate and admit the reconstruction.
    '''
    t = native.tools
    require(re.fullmatch(r'[0-9]+', str(run_id)) and re.fullmatch(r'[0-9]+', str(attempt)),
            'numeric checkpoint producer identity required')
    require(re.fullmatch(r'sha256:[0-9a-f]{64}|[0-9a-f]{64}', identity), 'immutable local payload required')
    t.inspect_local(identity, inputs['architecture'], identity)
    with t.mounted(identity) as root:
        # inventory skips normal artifact metadata; raw scratch must not carry
        # an unchecked metadata blob outside the full private-material audit.
        require(not (root / native.METADATA).exists() and not (root / native.METADATA).is_symlink(),
                'raw payload unexpectedly contains artifact metadata')
        inventory = native.inventory(root, inputs)
        inventory_raw = t.canonical(inventory)
        require(len(inventory_raw) <= 16 * 1024 * 1024, 'checkpoint inventory exceeds bound')
        repository = 'code.podesta.ai/qos/tilo-development-unapproved-native-payload-aarch64'
        require(inputs['architecture'] == 'aarch64', 'checkpoint architecture differs')
        reconstruction_raw = t.canonical(reconstruction)
        require(len(reconstruction_raw) <= 65536, 'checkpoint reconstruction evidence exceeds bound')
        (evidence / 'unapproved-reconstruction.json').write_bytes(reconstruction_raw)
        digest_file = work / 'unapproved-manifest.digest'
        t.command('podman', '--remote=false', 'push', '--tls-verify=true', '--digestfile', str(digest_file),
                  identity, 'docker://' + repository + ':candidate-' + str(run_id) + '-' + str(attempt))
        manifest = digest_file.read_text().strip()
        require(re.fullmatch(r'sha256:[0-9a-f]{64}', manifest), 'checkpoint manifest digest differs')
        reference = repository + '@' + manifest
        config = t.registry_identity(reference, inputs['architecture'])
        require(config.removeprefix('sha256:') == identity.removeprefix('sha256:'),
                'published unapproved payload differs')
        receipt = {'schema': 1, 'purpose': 'tilo-development-unapproved-native-payload-checkpoint',
            'inputs': inputs, 'reference': reference, 'config_digest': config,
            'inventory_sha256': t.sha(inventory_raw), 'inventory_count': len(inventory),
            'producer_run_id': str(run_id), 'producer_attempt': str(attempt),
            'reconstruction_evidence_sha256': t.sha(reconstruction_raw),
            'reconstruction_evidence_digest_format': 'json-sort-keys-compact-no-newline',
            'complete_native_builder': False, 'installable': False, 'production_signed': False,
            'consumer_adoption_required': True, 'producer_validation_complete': False}
        raw = t.canonical(receipt)
        require(len(raw) <= 65536, 'checkpoint receipt exceeds bound')
        (evidence / 'unapproved-payload.json').write_bytes(raw)
        # Original validation exceptions propagate; checkpoint receipt survives.
        yield root, inventory


def derive_driver(original, source_path, derived_recipe_path, seed_image_id):
    """Every variable substitution is public and retained in reconstruction proof."""
    require(PurePosixPath(source_path).is_absolute() and PurePosixPath(derived_recipe_path).is_absolute(), 'absolute reconstruction paths required')
    require(re.fullmatch(r'sha256:[0-9a-f]{64}', seed_image_id), 'immutable seed image required')
    text = replace_once(original, 'source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"',
        'source ' + shlex.quote(str(PurePosixPath(source_path) / 'os/dev/_common.sh')))
    # The builder target has no kernel dependency. Keep full-image selection
    # intact, but do not read an unrelated kernel pin for this native target.
    # scratch is an inert Dockerfile stage input, never a fabricated artifact.
    start = '_tilo_kernel_ref_override="${KERNEL_RPMS_REF:-}"\n'
    end = '# Pre-flight: every host-side COPY source exists.'
    require(text.count(start) == 1 and text.count(end) == 1, 'frozen kernel selection seam changed')
    kernel = text[text.index(start):text.index(end)]
    text = replace_once(text, kernel,
        'if [ "${TILO_NATIVE_BUILDER_ONLY:-0}" = 1 ]; then\n'
        '  KERNEL_RPMS_REF=scratch\n'
        'else\n' + kernel + 'fi\n')
    return replace_once(text, 'build -f "$TILO_OS_DIR/Containerfile"',
        'build -f ' + shlex.quote(str(derived_recipe_path)) + ' --build-context tilo-resume=container-image://' + seed_image_id)


def validate_partial(native, receipt, expected, builder, destination, union):
    t = native.tools
    require(set(receipt) == {'purpose', 'source_revision', 'reference', 'config_digest', 'metadata_sha256',
                            'inventory', 'installable', 'complete_native_builder', 'build_success'}, 'partial receipt schema differs')
    require(receipt['purpose'] == PARTIAL and receipt['source_revision'] == SOURCE and
            all(receipt[k] is False for k in ('installable', 'complete_native_builder', 'build_success')), 'not an honest partial checkpoint')
    require(re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-partial-native-aarch64@sha256:[0-9a-f]{64}', receipt['reference']), 'partial repository differs')
    require(t.registry_identity(receipt['reference'], 'aarch64') == receipt['config_digest'], 'partial registry config differs')
    t.command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/arm64', receipt['reference'])
    identity = t.inspect_local(receipt['reference'], 'aarch64', receipt['config_digest'])
    with t.mounted(identity) as root:
        metadata = t.read_bounded(root / 'partial-checkpoint.json', 1024 * 1024)
        require(t.sha(metadata) == receipt['metadata_sha256'], 'partial metadata hash differs')
        value = t.strict_json(metadata)
        require(value['schema'] == 1 and value['purpose'] == PARTIAL
                and all(value[k] is False for k in ('installable', 'complete_native_builder', 'build_success'))
                and value['source_revision'] == SOURCE and value['architecture'] == 'aarch64'
                and value['run_id'] == 20866 and value['run_number'] == 7489
                and value['inventory'] == receipt['inventory'], 'partial metadata identity differs')
        inputs = value['inputs']
        require(inputs['revision'] == SOURCE and inputs['architecture'] == 'aarch64' and inputs['builder'] == builder
                and inputs['pins']['BASE_REF'] == expected['base'] and inputs['pins']['RPMS_REF'] == expected['rpms']
                and inputs['wpe_adoption_sha256'] == expected['wpe_receipt_sha256'], 'partial source/toolchain inputs differ')
        partial = root / 'partial'
        require(partial.is_dir() and not partial.is_symlink(), 'invalid partial root')
        require(set(p.name for p in partial.iterdir()) == set(receipt['inventory']), 'partial file set differs')
        for name, record in receipt['inventory'].items():
            require(name in set(native.REQUIRED) | {'browser'}, 'unexpected partial binary')
            path = partial / name
            require(path.is_file() and not path.is_symlink() and path.stat().st_mode & 0o111, 'invalid partial ELF')
            actual, header = native.file_record(path)
            require(actual == record and len(header) == 64 and header[:7] == b'\x7fELF\x02\x01\x01'
                    and struct.unpack_from('<H', header, 18)[0] == 183
                    and struct.unpack_from('<H', header, 16)[0] in (2, 3), 'partial ELF bytes/architecture differ')
            require(name not in union or union[name] == record, 'overlapping checkpoint binaries differ')
            if name not in union:
                shutil.copyfile(path, destination / name)
                (destination / name).chmod(0o755)
                require(native.file_record(destination / name)[0] == record, 'copied checkpoint differs')
                union[name] = record


def check_complete(native, identity, inputs, metadata_sha256, reconstruction_sha256):
    t = native.tools
    with t.mounted(identity) as root:
        raw = t.read_bounded(root / native.METADATA, 16 * 1024 * 1024)
        require(t.sha(raw) == metadata_sha256, 'reconstructed metadata hash differs')
        value = t.strict_json(raw)
        require(set(value) == {'schema', 'purpose', 'inputs', 'inventory', 'reconstruction'}
                and value['schema'] == 1 and value['purpose'] == PURPOSE and value['inputs'] == inputs,
                'reconstructed complete metadata differs')
        require(t.sha(t.canonical(value['reconstruction'])) == reconstruction_sha256, 'reconstruction approval differs')
        require(value['inventory'] == native.inventory(root, inputs), 'complete native inventory differs')


def import_reconstructed(native, receipt_path, receipt_sha256, inputs, approved_reconstruction_sha256):
    """Separate opt-in consumer seam; never accepted by the legacy importer."""
    t = native.tools
    require(re.fullmatch('[0-9a-f]{64}', approved_reconstruction_sha256 or ''), 'independent reconstruction approval required')
    raw = t.read_bounded(Path(receipt_path), 1024 * 1024)
    require(t.sha(raw) == receipt_sha256, 'reconstructed receipt hash differs')
    receipt = t.strict_json(raw)
    require(set(receipt) == {'schema', 'purpose', 'inputs', 'reference', 'config_digest', 'metadata_sha256',
                            'reconstruction', 'reconstruction_sha256', 'complete_native_builder',
                            'production_signed', 'installable', 'consumer_adoption_required'}, 'reconstructed receipt schema differs')
    require(receipt['schema'] == 1 and receipt['purpose'] == PURPOSE and receipt['inputs'] == inputs
            and receipt['complete_native_builder'] is True and receipt['production_signed'] is False,
            'reconstructed receipt identity differs')
    require(receipt['installable'] is False and receipt['consumer_adoption_required'] is True
            and receipt['reconstruction_sha256'] == approved_reconstruction_sha256
            and t.sha(t.canonical(receipt['reconstruction'])) == approved_reconstruction_sha256,
            'reconstruction was not independently approved')
    require(re.fullmatch(re.escape(REPOSITORY) + '@sha256:[0-9a-f]{64}', receipt['reference']), 'reconstructed repository differs')
    t.native_host('aarch64')
    require(t.registry_identity(receipt['reference'], 'aarch64') == receipt['config_digest'], 'reconstructed registry config differs')
    t.command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/arm64', receipt['reference'])
    identity = t.inspect_local(receipt['reference'], 'aarch64', receipt['config_digest'])
    check_complete(native, identity, inputs, receipt['metadata_sha256'], approved_reconstruction_sha256)
    return identity


RECONSTRUCTED_PARTIAL = 'tilo-development-reconstructed-partial-native-checkpoint'
PARTIAL_HELPER = '9d7a6b259c9b75b6c5031472bfed05b6461c1893'


def validate_reconstructed_metadata(native, receipt, raw, approval, receipt_sha, source, legacy_checkpoints, legacy_union):
    """Depth-one opt-in: preserve producer purpose and independently approved chain."""
    t = native.tools
    require(isinstance(approval, dict), 'explicit reconstructed partial admission required')
    require(set(approval) == {'schema', 'purpose', 'source_revision', 'helper_revision', 'run_id', 'run_number',
        'attempt', 'reconstruction_sha256', 'resume_helper_sha256', 'producer_sha256', 'receipts'}, 'partial admission schema differs')
    require(approval['schema'] == 1 and approval['purpose'] == 'tilo-reconstructed-partial-admission'
        and approval['source_revision'] == SOURCE and approval['helper_revision'] == PARTIAL_HELPER
        and approval['run_id'] == 21554 and approval['run_number'] == 8166 and approval['attempt'] == 1,
        'unreviewed reconstructed partial producer')
    require(isinstance(approval['receipts'], dict) and 1 <= len(approval['receipts']) <= 2
        and receipt_sha in approval['receipts'], 'partial receipt not independently admitted')
    admitted = approval['receipts'][receipt_sha]
    require(set(admitted) == {'inventory', 'capture_script_sha256'}, 'partial admission entry differs')
    require(set(receipt) == {'schema','purpose','reference','config_digest','metadata_sha256','source_revision',
        'helper_revision','run_number','run_id','inventory','reconstruction_sha256','installable',
        'complete_native_builder','build_success','resume_admitted','raw_manifest_verified','raw_config_verified',
        'payload_hashes_verified','temporary_public_context_removed_on_exit'}, 'reconstructed receipt schema differs')
    require(receipt['schema'] == 1 and receipt['purpose'] == RECONSTRUCTED_PARTIAL
        and receipt['source_revision'] == SOURCE and receipt['helper_revision'] == PARTIAL_HELPER
        and receipt['run_id'] == 21554 and receipt['run_number'] == 8166
        and receipt['inventory'] == admitted['inventory'] and receipt['inventory'], 'reconstructed receipt identity differs')
    require(all(receipt[k] is False for k in ('installable','complete_native_builder','build_success','resume_admitted')),
        'reconstructed partial claimed completion/admission')
    require(t.sha(raw) == receipt['metadata_sha256'], 'reconstructed partial metadata differs')
    value = t.strict_json(raw)
    require(value['schema'] == 1 and value['purpose'] == RECONSTRUCTED_PARTIAL and value['architecture'] == 'aarch64'
        and all(value[k] is False for k in ('installable','complete_native_builder','build_success','resume_admitted','original_recipe_executed_verbatim'))
        and all(value[k] == approval[k] for k in ('source_revision','helper_revision','run_id','run_number','attempt'))
        and value['inventory'] == admitted['inventory'] and value['capture_script_sha256'] == admitted['capture_script_sha256'],
        'reconstructed metadata producer/capture differs')
    reconstruction = value['reconstruction']
    canonical_sha = t.sha(t.canonical(reconstruction))
    require(canonical_sha == approval['reconstruction_sha256'] == receipt['reconstruction_sha256']
        == value['reconstruction_sha256'], 'producer reconstruction approval differs')
    require(reconstruction['schema'] == 1 and reconstruction['purpose'] == 'tilo-native-reconstruction'
        and reconstruction['source_revision'] == SOURCE and reconstruction['run_id'] == '21554'
        and reconstruction['attempt'] == '1' and reconstruction['original_recipe_executed_verbatim'] is False
        and reconstruction['resume_helper_sha256'] == approval['resume_helper_sha256']
        and reconstruction['producer_sha256'] == approval['producer_sha256']
        and reconstruction['checkpoints'] == legacy_checkpoints and reconstruction['reused_inventory'] == legacy_union,
        'producer seed chain differs')
    original = (Path(source)/'os/Containerfile').read_text()
    original_driver = (Path(source)/'os/dev/build-image.sh').read_text()
    derived, missing = derive_recipe(original, legacy_union, native.REQUIRED)
    driver = derive_driver(original_driver, reconstruction['source_path'], reconstruction['derived_recipe_path'], reconstruction['seed_image_id'])
    for key, text in [('original_recipe_sha256',original),('derived_recipe_sha256',derived),
                      ('original_driver_sha256',original_driver),('derived_driver_sha256',driver)]:
        require(t.sha(text.encode()) == reconstruction[key], 'producer derivation differs: '+key)
    require(reconstruction['missing_targets'] == missing, 'producer missing targets differ')
    require(set(receipt['inventory']) <= set(missing['release']) | set(missing['windows']), 'partial contains unbuilt seed target')
    return value


def validate_reconstructed_partial(native, receipt, receipt_sha, approval, source, legacy_checkpoints, legacy_union, destination, union):
    t = native.tools
    require(re.fullmatch(r'code\.podesta\.ai/qos/tilo-development-partial-native-aarch64@sha256:[0-9a-f]{64}',receipt['reference']), 'partial repository differs')
    require(t.registry_identity(receipt['reference'],'aarch64') == receipt['config_digest'], 'partial registry config differs')
    t.command('podman','--remote=false','pull','--tls-verify=true','--platform=linux/arm64',receipt['reference'])
    identity=t.inspect_local(receipt['reference'],'aarch64',receipt['config_digest'])
    with t.mounted(identity) as root:
        raw=t.read_bounded(root/'partial-checkpoint.json',1024*1024)
        validate_reconstructed_metadata(native,receipt,raw,approval,receipt_sha,source,legacy_checkpoints,legacy_union)
        partial=root/'partial'
        require(partial.is_dir() and not partial.is_symlink() and {p.name for p in partial.iterdir()} == set(receipt['inventory']), 'partial file set differs')
        for name,record in receipt['inventory'].items():
            require(name in set(native.REQUIRED)|{'browser'},'unexpected partial binary')
            path=partial/name
            require(path.is_file() and not path.is_symlink() and path.stat().st_mode & 0o111,'invalid partial ELF')
            actual,header=native.file_record(path)
            require(actual == record and len(header)==64 and header[:7]==b'\x7fELF\x02\x01\x01'
                and struct.unpack_from('<H',header,18)[0]==183 and struct.unpack_from('<H',header,16)[0] in (2,3),'partial ELF differs')
            require(name not in union or union[name]==record,'overlapping checkpoint binaries differ')
            if name not in union:
                shutil.copyfile(path,destination/name);(destination/name).chmod(0o755)
                require(native.file_record(destination/name)[0]==record,'copied checkpoint differs')
                union[name]=record
