#!/usr/bin/env python3
"""Build and seal only the Browser's pinned engine through normal release tools."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = 'so.tilo.runtime.chromium'
MACHINES = {'x86_64': 62, 'aarch64': 183}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def engine_mode(environment, head):
    requested = environment.get('ENGINE_RUNTIME_ONLY', '')
    require(requested in ('', 'false', 'true'), 'invalid engine-only flag')
    if requested != 'true':
        return False
    require(environment.get('GITHUB_EVENT_NAME') == 'workflow_dispatch'
            and environment.get('GITHUB_REF') == 'refs/heads/main',
            'engine-only releases require an explicit main dispatch')
    require(re.fullmatch('[0-9a-f]{40}', head)
            and environment.get('GITHUB_SHA') == head, 'engine source must be the exact event commit')
    for name in ('COORDINATOR_INTENT', 'REQUESTED_BUILDER', 'BASELINE_SHA',
                 'BASELINE_RUN_ID', 'BASELINE_EVIDENCE', 'BASELINE_RECEIPT_SHA256',
                 'COORDINATOR_POLICY_SHA256'):
        require(not environment.get(name), 'engine-only release conflicts with ' + name)
    require(environment.get('VERIFY_FINAL_RECEIPT', '') in ('', 'false'),
            'collection receipt qualification is not an engine-only mode')
    require(environment.get('REQUESTED_COLLECTION', '') in ('', 'default'),
            'engine-only release does not select an app collection')
    require(environment.get('REQUESTED_APPS', '') in ('', 'browser'),
            'engine-only release does not select other apps')
    return True


def head():
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()


def runtime_version(root=ROOT):
    source = (root / 'crates/sdk/ui-core/src/webview/chromium/runtime.rs').read_text()
    version = re.search(r'pub const RUNTIME_VERSION: &str = "([^"]+)";', source)
    reference = re.search(r'pub const RUNTIME_REF: &str = "([^"]+)";', source)
    require(version and reference, 'missing engine runtime declaration')
    version = version.group(1)
    require(re.fullmatch('[0-9A-Za-z][0-9A-Za-z._-]{0,63}', version), 'invalid engine runtime version')
    require(reference.group(1) == 'tilo-runtime/' + version, 'engine runtime reference differs')
    manifest = tomllib.loads((root / 'apps/browser/tilo-app.toml').read_text())
    require(manifest['package']['optional-runtimes'] == ['tilo-runtime/' + version],
            'Browser optional runtime differs')
    recipe = (root / 'os/Containerfile').read_text()
    require(re.findall(r'^ARG CHROMIUM_RUNTIME_VERSION=(\S+)$', recipe, re.MULTILINE) == [version],
            'OS engine runtime differs')
    return version


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        while block := stream.read(1024 * 1024):
            value.update(block)
    return value.hexdigest()


def native_payload(root, architecture):
    require(architecture in MACHINES, 'unsupported native architecture')
    files = 0
    for path in root.rglob('*'):
        require(path.resolve().is_relative_to(root.resolve()), 'engine payload escapes its directory')
        if not path.is_file():
            continue
        with path.open('rb') as stream:
            header = stream.read(20)
        if header[:4] != b'\x7fELF':
            continue
        require(len(header) == 20 and header[4:6] == b'\x02\x01'
                and int.from_bytes(header[18:20], 'little') == MACHINES[architecture],
                'engine payload contains a foreign ELF')
        files += 1
    require(files > 0, 'engine payload contains no native ELF')


def provenance(architecture, source):
    require(source == head() and re.fullmatch('[0-9a-f]{40}', source), 'engine source commit differs')
    lock = json.loads((ROOT / 'scripts/browser/cef-lock.json').read_text())
    platform = {'aarch64': 'linuxarm64', 'x86_64': 'linux64'}[architecture]
    cef = lock['platforms'][platform]
    require(re.fullmatch('[0-9a-f]{64}', cef['sha256']) and type(cef['size']) is int and cef['size'] > 0,
            'invalid pinned CEF input')
    return {'schema': 1, 'source': source, 'architecture': architecture,
            'runtime': 'tilo-runtime/' + runtime_version(),
            'cef_version': lock['cef_version'], 'chromium_version': lock['chromium_version'],
            'cef_sha256': cef['sha256'], 'cef_size': cef['size']}


def cargo_tools():
    built = subprocess.run(['cargo', 'build', '--locked', '--release', '-p',
                            'tilo-app-package-cli', '-p', 'runtime-bundle',
                            '--message-format=json-render-diagnostics'], cwd=ROOT,
                           check=True, stdout=subprocess.PIPE, text=True)
    result = {}
    for line in built.stdout.splitlines():
        item = json.loads(line)
        name = item.get('target', {}).get('name')
        if item.get('reason') == 'compiler-artifact' and name in ('tilo-app-package', 'tilo-runtime-bundle'):
            executable = item.get('executable')
            if executable:
                result[name] = Path(executable).resolve(strict=True)
    require(set(result) == {'tilo-app-package', 'tilo-runtime-bundle'}, 'Cargo omitted release tools')
    return result


def compile_engine(out, architecture, source):
    proof = provenance(architecture, source)
    require(not subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=normal'],
                                        cwd=ROOT, text=True).strip(),
            'engine compilation requires a clean committed source')
    subprocess.run(['bash', 'os/dev/assert-native-arch.sh', architecture], cwd=ROOT, check=True)
    require(not out.exists(), 'engine release output already exists')
    out.mkdir(parents=True)
    tools = cargo_tools()
    binaries = out / 'bin'
    binaries.mkdir()
    for name, executable in tools.items():
        shutil.copy2(executable, binaries / name)
    platform = {'aarch64': 'linuxarm64', 'x86_64': 'linux64'}[architecture]
    subprocess.run([sys.executable, 'scripts/browser/build-host.py', '--platform', platform,
                    '--runtime', str(out / 'engine'), '--jobs', os.environ.get('CARGO_BUILD_JOBS', '2')],
                   cwd=ROOT, check=True)
    native_payload(out / 'engine', architecture)
    subprocess.run(['bash', 'os/scripts/verify-elf-hardening.sh',
                    str(out / 'engine/chromium/tilo-chromium-host'),
                    *[str(path) for path in binaries.iterdir()]], cwd=ROOT, check=True)
    proof['tools'] = {path.name: digest(path) for path in binaries.iterdir()}
    proof['host_sha256'] = digest(out / 'engine/chromium/tilo-chromium-host')
    (out / 'build.json').write_text(json.dumps(proof, sort_keys=True) + '\n')


def seal_engine(out, architecture, source, sequence):
    expected = provenance(architecture, source)
    proof = json.loads((out / 'build.json').read_text())
    require({key: proof.get(key) for key in expected} == expected, 'compiled engine provenance differs')
    require(proof['host_sha256'] == digest(out / 'engine/chromium/tilo-chromium-host'), 'engine host changed')
    require({path.name: digest(path) for path in (out / 'bin').iterdir()} == proof['tools'],
            'compiled release tools changed')
    native_payload(out / 'engine', architecture)
    require(re.fullmatch('[1-9][0-9]*', sequence), 'positive release sequence required')
    version = '0.1.' + sequence + '+g' + source
    binary = out / 'bin/tilo-runtime-bundle'
    identities = []
    for variable, name in (('RUNTIME_BUNDLE_IDENTITY_FILE', 'runtime'), ('TAPP_PUBLISHER_IDENTITY_FILE', 'app')):
        identity = os.environ.get(variable)
        if not identity:
            require(os.environ.get('REQUESTED_PUBLISH') == 'false', 'publication requires CI signing identities')
            private = out / 'work' / (name + '-identity.json')
            private.parent.mkdir(exist_ok=True)
            subprocess.run([str(binary), 'key', 'generate', '--out', str(private)], check=True)
            identity = str(private)
        identities.append(identity)
    runtime_identity, app_identity = identities
    runtime = out / 'runtime'
    subprocess.run([str(binary), 'build', '--version', runtime_version(), '--arch', architecture,
                    '--from', '/', '--include', str(out / 'engine/chromium'),
                    '--identity', runtime_identity, '--out', str(runtime)], check=True)
    runtime_key = subprocess.check_output([str(binary), 'key', 'pubkey', '--identity', runtime_identity], text=True).strip()
    subprocess.run([str(binary), 'verify', str(runtime), '--require-publisher', runtime_key], check=True)
    require(tomllib.loads((runtime / 'bundle.toml').read_text())['version'] == runtime_version(),
            'produced bundle runtime differs')
    native_payload(runtime, architecture)
    subprocess.run(['bash', 'tools/apps/pack-runtime-package.sh', str(runtime), PACKAGE,
                    'Browser engine', version, app_identity, str(out / 'bin'), str(out / 'package')],
                   cwd=ROOT, check=True)
    package_key = subprocess.check_output([str(binary), 'key', 'pubkey', '--identity', app_identity], text=True).strip()
    proof.update(package_version=version, runtime_publisher=runtime_key, app_publisher=package_key)
    proof['files'] = {path.name: digest(path) for path in (out / 'package').iterdir() if path.is_file()}
    (out / 'package/provenance.json').write_text(json.dumps(proof, sort_keys=True) + '\n')
    verify_package(out, architecture, source, sequence, package_key)
    # The canonical publisher entry verifier reconstructs every thin payload object.
    subprocess.run([str(out / 'bin/tilo-app-package'), 'entry', '--arch', architecture,
                    '--identity', app_identity, '--objects', str(out / 'package'),
                    '--url', 'https://apps.tilo.so/artifacts/engine.tapp',
                    str(next((out / 'package').glob('*.tapp')))],
                   check=True, stdout=subprocess.DEVNULL)


def verify_package(out, architecture, source, sequence, trusted_key):
    package = out / 'package'
    proof = json.loads((package / 'provenance.json').read_text())
    expected = provenance(architecture, source)
    require({key: proof.get(key) for key in expected} == expected, 'published engine provenance differs')
    require(proof['package_version'] == '0.1.' + sequence + '+g' + source, 'package source/version differs')
    require(proof['app_publisher'] == trusted_key, 'engine publisher differs from CI identity')
    inventory = {path.name: digest(path) for path in package.iterdir() if path.is_file() and path.name != 'provenance.json'}
    require(inventory == proof['files'], 'engine package inventory differs')
    tapps = list(package.glob('*.tapp'))
    require(len(tapps) == 1, 'one engine package required')
    verifier = Path(os.environ.get('TILO_ENGINE_VERIFIER_BIN_DIR', str(out / 'bin')))
    cli = verifier / 'tilo-app-package'
    subprocess.run([str(cli), 'verify', '--trusted-key', trusted_key, str(tapps[0])], check=True)
    inspected = subprocess.check_output([str(cli), 'inspect', str(tapps[0])], text=True)
    manifest = tomllib.loads(inspected.split('--- manifest.toml ---\n', 1)[1]
                             .split('--- payload (', 1)[0])
    require(manifest['package']['id'] == PACKAGE
            and manifest['package']['version'] == proof['package_version']
            and manifest['package']['provides'] == expected['runtime']
            and manifest['package']['kind'] == 'runtime'
            and manifest['package']['isa'] == {architecture: 'v3' if architecture == 'x86_64' else 'aarch64'},
            'produced tapp identity, version, runtime or architecture differs')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('plan', 'compile', 'seal', 'verify'))
    parser.add_argument('--out', type=Path, default=ROOT / 'dist/browser-runtime-release')
    parser.add_argument('--arch', choices=MACHINES)
    parser.add_argument('--source')
    parser.add_argument('--sequence')
    parser.add_argument('--trusted-key')
    arguments = parser.parse_args()
    if arguments.mode != 'plan':
        require(arguments.arch and arguments.source, 'architecture and source are required')
        if arguments.mode in ('seal', 'verify'):
            require(arguments.sequence and re.fullmatch('[1-9][0-9]*', arguments.sequence),
                    'positive release sequence required')
        if arguments.mode == 'verify':
            require(arguments.trusted_key and re.fullmatch('[0-9a-f]{64}', arguments.trusted_key),
                    'trusted publisher key required')
    if arguments.mode == 'plan':
        enabled = engine_mode(os.environ, head())
        if enabled:
            runtime_version()
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
            output.write('engine_runtime_only=' + str(enabled).lower() + '\n')
    elif arguments.mode == 'compile':
        compile_engine(arguments.out, arguments.arch, arguments.source)
    elif arguments.mode == 'seal':
        seal_engine(arguments.out, arguments.arch, arguments.source, arguments.sequence)
    else:
        verify_package(arguments.out, arguments.arch, arguments.source, arguments.sequence, arguments.trusted_key)


if __name__ == '__main__':
    main()
