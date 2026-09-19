"""Metadata-first bridge for an existing pinned public checkpoint exporter.

The remote exporter performs its ordinary image mounts and public validation, but
only its bounded metadata crosses this channel.  The signing spool carries each
large EFI input once; materialization joins those independently validated results.
Diagnostics never expose artifact bytes, exception messages, or keys.
"""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import traceback


WRAPPER = r'''
import json,sys,traceback
source=sys.argv[1];sys.argv=[sys.argv[0],sys.argv[2]]
output=sys.stdout
class Discard:
 def write(self,data):return len(data)
 def flush(self):pass
 @property
 def buffer(self):return self
sys.stdout=Discard()
try:
 scope={}
 exec(compile(source,'<checkpoint-export>','exec'),scope)
 result={'validated':True,'purpose':scope['purpose'],
         'input_sha256':scope['result']['input_sha256'],'input_bytes':scope['result']['input_bytes'],
         'checkpoint_metadata':scope['result']}
except Exception as error:
 frames=traceback.extract_tb(error.__traceback__)
 result={'validated':False,'exception_type':type(error).__name__,
         'locations':[{'file':f.filename.rsplit('/',1)[-1],'line':f.lineno,'function':f.name} for f in frames[-8:]]}
finally:sys.stdout=output
print(json.dumps(result,sort_keys=True))
'''


def _inspect(operation, directory, purpose, local_spool):
    """Run the pinned exporter's validations without transporting artifact bytes."""
    local_spool.private_directory(operation)
    local_spool.private_directory(directory)
    if directory.parent != operation or purpose not in ('loader', 'live-uki', 'target-uki'):
        raise ValueError('checkpoint diagnostic scope differs')
    config = local_spool.document(local_spool.signer.trusted(operation / 'operation-config.json'))
    operator = operation / 'operator'
    # Both modules are code, so authenticate each before importing either.
    for name in ('finalization_operation.py', 'export-finalization-checkpoint.py'):
        raw = local_spool.snapshot(local_spool.signer.trusted(operator / name), 2**20)
        if hashlib.sha256(raw).hexdigest() != config['operator_sha256'][name]:
            raise ValueError('pinned checkpoint operator changed')
    sys.path.insert(0, str(operator))
    spec = importlib.util.spec_from_file_location('pinned_checkpoint_exporter', operator / 'export-finalization-checkpoint.py')
    exporter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(exporter)
    exporter.OP = operation
    broker, stage, files = exporter.load_helpers()
    spool = stage.spool
    binding = spool.document(directory / 'binding.json')
    context = spool.document(directory / 'signing-context.json')
    bound = spool.document(directory / 'bound.json')
    stage.bound_context(context, binding)
    request_path = spool.signer.trusted(Path(bound['request_path']))
    if not request_path.is_relative_to(operation):
        raise ValueError('request lies outside operation')
    raw = spool.snapshot(request_path, spool.MAX_JSON)
    request = stage.driver.validate_request(stage.parse(raw))
    if (spool.digest(raw) != bound['request_sha256'] or spool.context(context) != bound['context_sha256']
            or request['helpers_revision'] != exporter.HELPER or request['source_revision'] != exporter.SOURCE
            or context['revision'] != request['source_revision'] or context['architecture'] != request['architecture']
            or context['manifest_sha256'] != request['new_manifest_sha256'] or context['target'] != request['new_target']
            or binding['docker']['image_id'] != exporter.TOOLS):
        raise ValueError('checkpoint diagnostic binding differs')
    pins = dict(purpose=purpose, helper_revision=exporter.HELPER, source_revision=exporter.SOURCE,
                request_sha256=bound['request_sha256'], helper_files=files, recipes=exporter.RECIPES)
    extra = ['docker', 'exec', '-i', binding['docker']['id'], '/usr/bin/python3', '-c', WRAPPER,
             exporter.REMOTE, spool.canonical(pins).decode()]

    class DiagnosticRunner(broker.BrokerRunner):
        def operation(self, argv):
            head = ['kubectl', '--context=' + broker.CONTEXT, '--namespace=' + binding['namespace'],
                    'exec', '-i', binding['pod'], '-c', binding['container'], '--']
            if argv == head + extra:
                return 'exec', extra
            return super().operation(argv)

    runner = DiagnosticRunner(binding)
    client = broker.transport.Transport(broker.CONTEXT, binding, runner=runner)
    output = io.BytesIO()
    client.check()
    runner(client.exec_prefix() + extra, output, 65536, timeout=600)
    client.check()
    result = json.loads(output.getvalue(), object_pairs_hook=spool.unique)
    metadata = None
    if result['validated']:
        metadata = result.pop('checkpoint_metadata')
        try:
            exporter.validate_metadata(metadata, request, binding, purpose, bound['request_sha256'], spool)
            result['local_metadata_validated'] = True
        except Exception as error:
            result.update(validated=False, phase='local-metadata', exception_type=type(error).__name__,
                locations=[{'file': Path(f.filename).name, 'line': f.lineno, 'function': f.name}
                           for f in traceback.extract_tb(error.__traceback__)[-8:]])
            metadata = None
    return result, metadata, spool, binding


def diagnose(operation, directory, purpose, local_spool):
    """Bridge legacy immutable operations without rewriting their active config."""
    result, _, _, _ = _inspect(operation, directory, purpose, local_spool)
    return result


def export_metadata(operation, directory, purpose, local_spool):
    """Publish an authenticated checkpoint receipt without its large EFI bytes."""
    result, metadata, spool, binding = _inspect(operation, directory, purpose, local_spool)
    if not result.get('validated') or not result.get('local_metadata_validated') or metadata is None:
        raise ValueError('checkpoint metadata validation failed')
    destination = directory / ('checkpoint-' + purpose)
    if destination.exists() or destination.is_symlink():
        raise ValueError('fresh checkpoint output required')
    ready = directory / ('.checkpoint-' + purpose + '.ready')
    if ready.exists() or ready.is_symlink():
        raise ValueError('checkpoint staging path already exists')
    ready.mkdir(mode=0o700)
    metadata['transport_context_sha256'] = binding['context_sha256']
    spool.write_new(ready / 'receipt.json', spool.canonical(metadata))
    ready.rename(destination)
    spool.sync_directory(directory)
    return {'directory': str(destination), 'purpose': purpose,
            'input_sha256': metadata['input_sha256'], 'input_bytes': metadata['input_bytes'],
            'metadata_only': True, 'signing_authorized': False}


def materialize(operation, directory, purpose, request_directory, local_spool):
    """Attach a fetched signing-spool input only when it matches the checkpoint."""
    local_spool.private_directory(operation)
    local_spool.private_directory(directory)
    local_spool.private_directory(request_directory)
    if directory.parent != operation or purpose not in ('live-uki', 'target-uki'):
        raise ValueError('checkpoint materialization scope differs')
    allowed = (directory / 'received', operation / 'received')
    if request_directory.parent not in allowed:
        raise ValueError('request directory lies outside admitted receive storage')
    destination = local_spool.private_directory(directory / ('checkpoint-' + purpose))
    receipt = local_spool.document(destination / 'receipt.json')
    binding = local_spool.document(directory / 'binding.json')
    context = local_spool.document(directory / 'signing-context.json')
    request = local_spool.request_metadata(
        local_spool.document(request_directory / 'request.json'))
    stage_input = local_spool.snapshot(request_directory / 'input', local_spool.MAX_INPUT)
    if (receipt.get('purpose') != purpose
            or receipt.get('transport_context_sha256') != binding['context_sha256']
            or receipt.get('signing_authorized') is not False
            or receipt.get('private_keys_used') is not False
            or request['request_id'] != request_directory.name
            or request['operation'] != 'sign-efi'
            or request.get('context_sha256') != binding['context_sha256']
            or request.get('context_sha256') != local_spool.context(context)
            or request.get('input_sha256') != receipt.get('input_sha256')
            or request.get('input_bytes') != receipt.get('input_bytes')
            or len(stage_input) != receipt.get('input_bytes')
            or local_spool.digest(stage_input) != receipt.get('input_sha256')):
        raise ValueError('fetched request differs from authenticated checkpoint')
    if (destination / 'input.efi').exists() or (destination / 'materialization.json').exists():
        raise ValueError('checkpoint materialization already exists')
    local_spool.write_new(destination / 'input.efi', stage_input)
    record = {'schema': 1, 'purpose': purpose, 'request_id': request['request_id'],
              'context_sha256': request['context_sha256'], 'input_sha256': request['input_sha256'],
              'input_bytes': request['input_bytes'], 'signing_authorized': False}
    local_spool.write_new(destination / 'materialization.json', local_spool.canonical(record))
    local_spool.sync_directory(destination)
    return {'directory': str(destination), 'purpose': purpose, 'request_id': request['request_id'],
            'input_sha256': request['input_sha256'], 'input_bytes': request['input_bytes'],
            'materialized': True, 'signing_authorized': False}
