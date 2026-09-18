"""One-shot, file-mediated remote signing; transport is deliberately external.

Only a local root-owned authorization policy permits private-key use. Remote
requests are data, not authority. No network listener or credential transfer.
"""
import hashlib
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import stat
import time

SPEC = importlib.util.spec_from_file_location('spool_signer', Path(__file__).with_name('development-signer.py'))
signer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signer)
MAX_INPUT = 512 * 1024**2
MAX_JSON = 65536
OPS = {'sign-efi', 'sign-sha256-rsa-pkcs1'}
HEX = re.compile(r'[0-9a-f]{64}')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def snapshot(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or not 0 < before.st_size <= limit:
            raise ValueError('bounded singly-linked regular signing file required')
        result = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        if len(result) != before.st_size or (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError('signing file changed during snapshot')
        return result


def unique(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('duplicate signing metadata field')
        value[key] = item
    return value


def document(path):
    try:
        value = json.loads(snapshot(path, MAX_JSON), object_pairs_hook=unique)
    except (UnicodeError, json.JSONDecodeError):
        raise ValueError('invalid signing metadata') from None
    if not isinstance(value, dict):
        raise ValueError('signing metadata must be an object')
    return value


def private_directory(path):
    path = signer.trusted(Path(path), directory=True)
    if path.stat().st_mode & 0o077:
        raise ValueError('signing spool directory must be owner-only')
    return path


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def write_new(path, data):
    with signer.exclusive_output(Path(path)) as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    sync_directory(Path(path).parent)


def publish(path, data):
    # Consumer watches only the final filename. Never overwrite a response.
    temporary = path.with_name('.pending-' + secrets.token_hex(16))
    write_new(temporary, data)
    try:
        # Linux renameat2(RENAME_NOREPLACE) publishes a single-link file in
        # one step. link+unlink exposes st_nlink=2 to concurrent readers.
        libc = ctypes.CDLL(None, use_errno=True)
        rename = libc.renameat2
        rename.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint)
        rename.restype = ctypes.c_int
        if rename(-100, os.fsencode(temporary), -100, os.fsencode(path), 1):
            error = ctypes.get_errno()
            raise OSError(error, 'exclusive signing publication failed')
        sync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def context(value):
    expected = {'schema', 'job_id', 'pod_uid', 'container_id', 'image_id',
                'revision', 'architecture', 'manifest_sha256', 'target'}
    if set(value) != expected or value['schema'] != 1:
        raise ValueError('invalid externally pinned signing context')
    if value['architecture'] not in ('aarch64', 'x86_64'):
        raise ValueError('unsupported signing architecture')
    if (not re.fullmatch(r'[0-9a-f]{40}', value['revision'])
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', value['image_id'])
            or not HEX.fullmatch(value['manifest_sha256'])):
        raise ValueError('signing context lacks immutable identities')
    for name in ('job_id', 'pod_uid', 'container_id', 'target'):
        if not isinstance(value[name], str) or not 1 <= len(value[name]) <= 512 or any(ord(c) < 33 for c in value[name]):
            raise ValueError('invalid signing context identifier')
    return digest(canonical(value))


def request_metadata(value):
    if (set(value) != {'schema', 'request_id', 'context_sha256', 'operation', 'input_sha256', 'input_bytes'}
            or value['schema'] != 1 or value['operation'] not in OPS
            or not re.fullmatch(r'[0-9a-f]{32}', value['request_id'])
            or not HEX.fullmatch(value['context_sha256']) or not HEX.fullmatch(value['input_sha256'])
            or type(value['input_bytes']) is not int or not 0 < value['input_bytes'] <= MAX_INPUT):
        raise ValueError('invalid signing request')
    return value


def remote_sign(config_path, operation, source, destination):
    config = document(signer.trusted(Path(config_path)))
    if set(config) != {'schema', 'context', 'spool', 'public_certificate', 'certificate_sha256', 'timeout_seconds'} or config['schema'] != 1:
        raise ValueError('invalid remote signer configuration')
    binding = context(config['context'])
    spool = private_directory(config['spool'])
    timeout = config['timeout_seconds']
    if type(timeout) is not int or not 1 <= timeout <= 1800:
        raise ValueError('invalid remote signing timeout')
    if operation == 'export-efi-cert':
        data = snapshot(signer.trusted(Path(config['public_certificate'])), 65536)
        if digest(data) != config['certificate_sha256']:
            raise ValueError('remote public certificate changed')
        write_new(Path(destination), data)
        return
    if operation not in OPS:
        raise ValueError('unsupported remote signing operation')
    data = snapshot(source, MAX_INPUT)
    nonce = secrets.token_hex(16)
    directory = spool / nonce
    directory.mkdir(mode=0o700)
    request = {'schema': 1, 'request_id': nonce, 'context_sha256': binding, 'operation': operation,
               'input_sha256': digest(data), 'input_bytes': len(data)}
    write_new(directory / 'input', data)
    publish(directory / 'request.json', canonical(request))
    deadline = time.monotonic() + timeout
    while not (directory / 'response.json').exists():
        if time.monotonic() >= deadline:
            raise ValueError('operator signing response timed out; request remains available')
        time.sleep(0.2)
    response = document(directory / 'response.json')
    expected = {'schema', 'request_id', 'context_sha256', 'operation', 'input_sha256', 'output_sha256', 'output_bytes'}
    if set(response) != expected or any(response[k] != request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')):
        raise ValueError('signing response does not bind this request')
    result = snapshot(directory / 'output', MAX_INPUT + 65536)
    if response['output_bytes'] != len(result) or response['output_sha256'] != digest(result):
        raise ValueError('signing response bytes differ')
    write_new(Path(destination), result)
    publish(directory / 'consumed.json', canonical({'request_id': nonce, 'output_sha256': digest(result)}))


def service_local_request(request_directory, policy_path, state_directory, response_directory, signer_path):
    """Sign one exact locally authorized input. Authentication/transport is external.

    Policy must be prepared from independently verified local artifact snapshots,
    never copied from request metadata. Every authorization is single-use.
    """
    request_directory = private_directory(request_directory)
    response_directory = private_directory(response_directory)
    state_directory = private_directory(state_directory)
    policy = document(signer.trusted(Path(policy_path)))
    if set(policy) != {'schema', 'context', 'authorized_inputs', 'efi_certificate', 'pcr_public_key'} or policy['schema'] != 1:
        raise ValueError('invalid local signing policy')
    binding = context(policy['context'])
    public = {}
    for name in ('efi_certificate', 'pcr_public_key'):
        key = policy[name]
        if not isinstance(key, dict) or set(key) != {'path', 'sha256'} or not HEX.fullmatch(key['sha256']):
            raise ValueError('independently pinned public signing keys required')
        public[name] = snapshot(signer.trusted(Path(key['path'])), 65536)
        if digest(public[name]) != key['sha256']:
            raise ValueError('local verification key changed')
    rows = policy['authorized_inputs']
    if not isinstance(rows, list) or not 1 <= len(rows) <= 64:
        raise ValueError('bounded explicit signing authorizations required')
    identifiers = set()
    for row in rows:
        if (not isinstance(row, dict) or set(row) != {'id', 'operation', 'input_sha256', 'input_bytes', 'purpose'}
                or not re.fullmatch(r'[a-z0-9-]{1,64}', row['id']) or row['id'] in identifiers
                or row['operation'] not in OPS or not HEX.fullmatch(row['input_sha256'])
                or type(row['input_bytes']) is not int or not 0 < row['input_bytes'] <= MAX_INPUT
                or row['purpose'] not in ('loader', 'uki', 'pcr-policy', 'detached-uki')):
            raise ValueError('invalid local signing authorization')
        if ((row['purpose'] in ('loader', 'uki')) != (row['operation'] == 'sign-efi')
                or (row['purpose'] == 'pcr-policy' and row['input_bytes'] != 40)
                or (row['purpose'] == 'loader' and row['input_bytes'] > 8 * 1024**2)):
            raise ValueError('signing purpose and operation disagree')
        identifiers.add(row['id'])
    request = request_metadata(document(request_directory / 'request.json'))
    if request['context_sha256'] != binding:
        raise ValueError('request belongs to another signing context')
    data = snapshot(request_directory / 'input', MAX_INPUT)
    if len(data) != request['input_bytes'] or digest(data) != request['input_sha256']:
        raise ValueError('request input changed')
    matches = [row for row in rows if all(row[k] == request[k] for k in ('operation', 'input_sha256', 'input_bytes'))]
    if not matches:
        raise ValueError('request input is not explicitly authorized locally')
    trusted_signer = signer.trusted(Path(signer_path))
    if not os.access(trusted_signer, os.X_OK):
        raise ValueError('local signer is not executable')
    # Durable reservation before key use. Failure requires explicit operator
    # resolution, never automatic replay after a transport timeout.
    used = state_directory / binding
    used.mkdir(mode=0o700, exist_ok=True)
    private_directory(used)
    nonce = used / ('request-' + request['request_id'])
    nonce.mkdir(mode=0o700)
    sync_directory(used)
    sync_directory(state_directory)
    selected = None
    for row in matches:
        try:
            reservation = used / ('authorization-' + row['id'])
            reservation.mkdir(mode=0o700)
            selected = row
            break
        except FileExistsError:
            continue
    if selected is None:
        raise ValueError('signing authorization was already consumed')
    sync_directory(used)
    write_new(reservation / 'request.json', canonical(request))
    local_input = reservation / 'input'
    local_output = reservation / 'output'
    write_new(local_input, data)
    signer.run([str(trusted_signer), request['operation'], str(local_input), str(local_output)])
    result = snapshot(local_output, MAX_INPUT + 65536)
    public_path = reservation / 'verification.pem'
    write_new(public_path, public['efi_certificate' if request['operation'] == 'sign-efi' else 'pcr_public_key'])
    if request['operation'] == 'sign-efi':
        import development_boot as boot
        boot.pe.verify(str(local_input), str(local_output))
        listing = signer.run(['/usr/bin/sbverify', '--list', str(local_output)])
        if not re.search(rb'^signature [1-9][0-9]*\s*$', listing, re.MULTILINE):
            raise ValueError('signed EFI lacks a recognized signature')
        signer.run(['/usr/bin/sbverify', '--cert', str(public_path), str(local_output)])
    else:
        if not 256 <= len(result) <= 1024:
            raise ValueError('RSA response size invalid')
        signer.run(['/usr/bin/openssl', 'dgst', '-sha256', '-verify', str(public_path),
                    '-sigopt', 'rsa_padding_mode:pkcs1', '-signature', str(local_output), str(local_input)])
    response = {k: request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
    response.update(output_sha256=digest(result), output_bytes=len(result))
    write_new(response_directory / 'output', result)
    publish(response_directory / 'response.json', canonical(response))
    publish(reservation / 'complete.json', canonical(response))
    return {'request_id': request['request_id'], 'authorization_id': selected['id'],
            'operation': request['operation'], 'input_sha256': request['input_sha256'],
            'output_sha256': digest(result), 'private_keys_transferred': False}
