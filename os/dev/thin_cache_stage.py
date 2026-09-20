"""Acquire public incomplete-cache inputs into a reviewed read-only build context."""
import hashlib
import json
from pathlib import Path
import re
import shutil


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def stage(native, approval_path, approval_sha, work):
    t = native.tools
    raw = t.read_bounded(Path(approval_path), 65536)
    if not re.fullmatch('[0-9a-f]{64}', approval_sha) or t.sha(raw) != approval_sha:
        raise ValueError('independent thin cache approval hash differs')
    approval = t.strict_json(raw)
    if (approval['purpose'] != 'tilo-incomplete-thin-cache-admission' or
            approval['source_revision'] != '4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc' or
            approval['producer_helper_revision'] != '9d7a6b259c9b75b6c5031472bfed05b6461c1893' or
            approval['incomplete_cache_reviewed'] is not True or
            approval['cargo_revalidation_required'] is not True):
        raise ValueError('thin cache admission identity differs')
    helper = Path(__file__).with_name('restore-thin-cache.py')
    if sha(helper) != approval['restore_helper_sha256']:
        raise ValueError('thin cache restore helper differs')
    destination = work / 'thin-cache-context'
    destination.mkdir()
    for prefix, repository, files in (
        ('checkpoint', 'tilo-development-partial-thin-cache-aarch64',
         [('thin-cache.tar', 'archive_sha256', 10*1024**3+128*1024**2),
          ('cache-metadata.json', 'metadata_sha256', 32*1024**2)]),
        ('source_metadata', 'tilo-development-thin-cache-metadata-aarch64',
         [('source-metadata.json', 'source_metadata_sha256', 32*1024**2)])):
        reference = approval[prefix + '_reference']
        config = approval[prefix + '_config_digest']
        if (not re.fullmatch('code\\.podesta\\.ai/tilo/' + repository + '@sha256:[0-9a-f]{64}', reference)
                or not re.fullmatch('sha256:[0-9a-f]{64}', config)
                or t.registry_identity(reference, 'aarch64') != config):
            raise ValueError('thin cache immutable registry identity differs')
        t.command('podman', '--remote=false', 'pull', '--tls-verify=true', '--platform=linux/arm64', reference)
        identity = t.inspect_local(reference, 'aarch64', config)
        with t.mounted(identity) as root:
            for name, field, bound in files:
                source = root / 'checkpoint' / name
                if (not source.is_file() or source.is_symlink() or source.resolve() != source or
                        source.stat().st_size > bound or sha(source) != approval[field]):
                    raise ValueError('thin cache public file differs: ' + name)
                shutil.copyfile(source, destination / name)
                if sha(destination / name) != approval[field]:
                    raise ValueError('staged thin cache file differs')
    shutil.copyfile(helper, destination / helper.name)
    (destination / 'approval.json').write_bytes(raw)
    return destination, dict(approval_sha256=approval_sha, approval=approval,
                             purpose='tilo-admitted-incomplete-thin-cache', cargo_revalidation_required=True,
                             dependency_timestamps_restored=False, complete=False, installable=False)
