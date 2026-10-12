"""Package a candidate SDK deterministically with pinned public CEF machinery.
This creates an UNQUALIFIED private artifact. It never signs, publishes, updates
consumer pins or claims qualification. Run under the remote bounded supervisor.
The packaging and validation algorithm is the one used by the actual A/B producer.
"""
from pathlib import Path
import datetime
import hashlib
import json
import os
import runpy
import subprocess
import sys
import struct
import tarfile

if len(sys.argv) != 3 or sys.argv[2] not in ['A', 'B']:
    raise SystemExit('usage: package_candidate.py OWNED_REMOTE_TASK_ROOT A|B')
assert os.environ.get('TILO_CEF_REQUIRE_MEMORY_ACCOUNTING') == '1', 'Run under the remote bounded memory-accounting supervisor'
ROOT = Path(sys.argv[1]).resolve(strict=True)
assert ROOT.is_dir() and not Path(sys.argv[1]).is_symlink()
CONFIG = Path(__file__).resolve().parent
phase = sys.argv[2]
recipe = json.loads((CONFIG / 'recipe.json').read_text())
check = {'repo_paths': recipe['source']['repo_paths'],
         'candidate_sha256': recipe['source']['candidate_file_sha256'],
         'patch_sha256': recipe['source']['patch_sha256']}
assert hashlib.sha256((CONFIG / 'renderer-guard.patch').read_bytes()).hexdigest() == check['patch_sha256']
assert (ROOT / 'artifact-args.gn').read_bytes() == (CONFIG / 'artifact-args.gn').read_bytes(), 'Use the reviewed production profile exactly'
SRC = ROOT / 'download/chromium/src'
for directory, expected in [(SRC, '79460ebecaa5625e57a5fb679a735659e73dc687'),
                            (SRC / 'cef', '708dc140cbc3286826a8abef89dc23a44ff9ea72')]:
    assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=directory, text=True).strip() == expected
for name, relative in check['repo_paths'].items():
    assert hashlib.sha256((SRC / relative).read_bytes()).hexdigest() == check['candidate_sha256'][name]
epoch = int(subprocess.check_output(['git', 'show', '-s', '--format=%ct', 'HEAD'], cwd=SRC, text=True))
os.environ.update({'SOURCE_DATE_EPOCH': str(epoch), 'TZ': 'UTC', 'LC_ALL': 'C.UTF-8',
                   'DEPOT_TOOLS_UPDATE': '0', 'DEPOT_TOOLS_METRICS': '0'})
# Pinned CEF date_util ignores SOURCE_DATE_EPOCH. Freeze only its documented
# date/year metadata functions, without editing source, headers or licenses.
sys.path.insert(0, str(SRC / 'cef/tools'))
import date_util
frozen = datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc)
date_util.get_date = lambda: frozen.strftime('%B %d, %Y')
date_util.get_year = lambda: str(frozen.year)
built = json.loads((ROOT / ('production-build-' + phase + '.json')).read_text())
assert built['patch_sha256'] == check['patch_sha256'] and built['pass'] == phase
assert built['args_sha256'] == hashlib.sha256((ROOT / 'artifact-args.gn').read_bytes()).hexdigest()
os.umask(0o022)
packages = ROOT / ('candidate-packages-' + phase)
assert not packages.exists(), 'Preserve previous candidate evidence'
packages.mkdir()
temporary = packages / '.tilo-tmp'
temporary.mkdir(exist_ok=True)
os.environ['TMPDIR'] = str(temporary)
sys.argv = [str(SRC / 'cef/tools/make_distrib.py'), '--ninja-build', '--x64-build',
            '--minimal', '--no-symbols', '--no-docs', '--no-archive',
            '--distrib-subdir-suffix=tilo-' + check['patch_sha256'][:16],
            '--output-dir=' + str(packages)]
try:
    runpy.run_path(sys.argv[0], run_name='__main__')
except SystemExit as exit_status:
    assert exit_status.code in [None, 0], exit_status.code
sdk_dirs = [p for p in packages.iterdir() if p.is_dir() and p.name != '.tilo-tmp']
assert len(sdk_dirs) == 1
sdk = sdk_dirs[0]
for relative in ['Release/libcef.so', 'Release/chrome-sandbox', 'include/cef_api_hash.h',
                 'include/cef_version.h', 'LICENSE.txt', 'README.txt', 'Resources/icudtl.dat',
                 'Resources/resources.pak', 'Resources/locales', 'libcef_dll']:
    assert (sdk / relative).exists(), 'Missing SDK input: ' + relative
# The pinned public packager copies this exact upstream CEF license verbatim.
# Check bytes against the revision-verified source, not mere presence.
license_sha256 = hashlib.sha256((SRC / 'cef/LICENSE.txt').read_bytes()).hexdigest()
assert hashlib.sha256((sdk / 'LICENSE.txt').read_bytes()).hexdigest() == license_sha256, 'SDK license changed from pinned CEF source'
# Keep every file the existing consumer requires, and reject a foreign ELF.
for relative in ['Release/libcef.so', 'Release/libEGL.so', 'Release/libGLESv2.so',
                 'Release/libvk_swiftshader.so', 'Release/libvulkan.so.1',
                 'Release/vk_swiftshader_icd.json', 'Release/v8_context_snapshot.bin',
                 'Resources/chrome_100_percent.pak', 'Resources/chrome_200_percent.pak',
                 'include/cef_app.h']:
    assert (sdk / relative).is_file(), 'Missing required consumer input: ' + relative
    if relative.endswith(('.so', '.so.1')):
        with (sdk / relative).open('rb') as stream:
            header = stream.read(20)
        assert header[:6] == b'\x7fELF\x02\x01' and struct.unpack_from('<H', header, 18)[0] == 62, 'SDK must contain x86-64 ELF: ' + relative
for relative, expected in {
    'include/cef_api_hash.h': '7b379865bb88e5c91e4ef5badd5f6769faed5bb230b8d30975bb7f8bd2f906cb',
    'include/cef_version.h': '972f38143256d3804aa9df2be5b23ca6a9e0b886304c4f0c4d4a2bcb54614429',
}.items():
    assert hashlib.sha256((sdk / relative).read_bytes()).hexdigest() == expected, 'Public SDK header parity failed: ' + relative
archive = packages / (sdk.name + '.tar.bz2')
inventory = []
with tarfile.open(archive, 'w:bz2', compresslevel=9) as tar:
    for path in [sdk, *sorted(sdk.rglob('*'))]:
        relative = path.relative_to(packages)
        assert not relative.is_absolute() and '..' not in relative.parts
        info = tar.gettarinfo(str(path), arcname=str(relative))
        info.uid = info.gid = 0
        info.uname = info.gname = ''
        info.mtime = epoch
        info.pax_headers = {}
        if path.is_symlink():
            assert path.resolve().is_relative_to(sdk.resolve()), 'SDK link escapes package'
            tar.addfile(info)
            inventory.append({'path': str(relative), 'link': str(path.readlink()), 'mode': info.mode})
        elif path.is_file():
            # Avoid inode-dependent hardlink encoding across independent builds.
            info.type = tarfile.REGTYPE
            info.linkname = ''
            info.size = path.stat().st_size
            with path.open('rb') as stream:
                digest = hashlib.file_digest(stream, 'sha256').hexdigest()
                stream.seek(0)
                tar.addfile(info, stream)
            inventory.append({'path': str(relative), 'sha256': digest, 'size': info.size, 'mode': info.mode})
        elif path.is_dir():
            tar.addfile(info)
        else:
            raise ValueError('Unsupported SDK entry: ' + str(relative))
with archive.open('rb') as stream:
    archive_hash = hashlib.file_digest(stream, 'sha256').hexdigest()
with archive.open('rb') as stream:
    archive_sha1 = hashlib.file_digest(stream, 'sha1').hexdigest()
receipt = {'status': 'UNQUALIFIED_DO_NOT_PUBLISH_OR_CONSUME', 'source_patch_sha256': check['patch_sha256'],
           'source_date_epoch': epoch, 'production_pass': phase, 'producer_profile_sha256': built['args_sha256'], 'date_util_metadata_frozen': True, 'archive': archive.name,
           'archive_sha256': archive_hash, 'archive_sha1': archive_sha1, 'license_parity': 'PASS_PINNED_CEF_SOURCE_BYTES', 'license_sha256': license_sha256, 'public_header_parity': 'PASS', 'required_runtime_files': 'PASS', 'ELF_architecture': 'x86_64', 'archive_size': archive.stat().st_size,
           'inventory': inventory, 'binary_in_git': False}
(packages / 'candidate-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps({k:v for k,v in receipt.items() if k != 'inventory'}), flush=True)
