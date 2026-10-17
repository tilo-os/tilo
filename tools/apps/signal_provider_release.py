#!/usr/bin/env python3
"""Build the optional Signal service with the existing protected release authorities."""
import argparse
import base64
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load('signal_release_tools', ROOT / 'tools/apps/browser_runtime_release.py')
fixture = load('signal_release_assets', ROOT / 'tools/apps/signal_runtime_fixture.py')
prepare = load('signal_release_source', ROOT / 'os/scripts/prepare-signal-provider.py')
zero = load('signal_zero_runtime', ROOT / 'tools/apps/signal_zero_runtime.py')
require = release.require
RUNTIME = 'tilo-runtime/2026.10.1-signal'
RUNTIME_ENTRYPOINT = 'lib/lib64/ld-linux-x86-64.so.2'


mode = release.signal_mode


def provenance(source):
    commit = release.verify_source(source)
    pin = json.loads(prepare.PIN.read_text())
    require(RUNTIME == 'tilo-runtime/' + pin['runtime'], 'pinned Signal runtime differs')
    manifest = tomllib.loads((ROOT / 'crates/packaging/launcher/signal-provider.toml').read_text())
    require(manifest['package']['runtime'] == RUNTIME, 'provider runtime declaration differs')
    require(manifest['package']['kind'] == 'service', 'provider must declare its headless service kind')
    require(manifest['sandbox']['seccomp'] == 'tilo-default', 'Signal must retain its strict sandbox')
    publisher = (ROOT / 'os/registry-runtime-publisher.hex').read_text().strip()
    require(re.fullmatch('[0-9a-f]{64}', publisher), 'accepted runtime publisher is missing')
    return {'source': commit, 'architecture': 'x86_64', 'runtime': RUNTIME,
            'pin': pin, 'runtime_publisher': publisher}


def inventory(path):
    result = {}
    for entry in path.rglob('*'):
        require(not entry.is_symlink(), 'release inventory contains a link')
        if entry.is_file():
            result[str(entry.relative_to(path))] = release.digest(entry)
    require(result, 'release inventory is empty')
    return result


def compile_provider(out, source):
    proof = provenance(source)
    if not isinstance(source, release.Source) or source.snapshot_commit is None:
        require(not subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True).strip(),
                'Signal compilation requires a clean committed source')
    subprocess.run(['bash', 'os/dev/assert-native-arch.sh', 'x86_64'], cwd=ROOT, check=True)
    require(not out.exists(), 'Signal release output already exists')
    out.mkdir(parents=True)
    binaries = out / 'bin'
    binaries.mkdir()
    for name, executable in release.cargo_tools().items():
        shutil.copy2(executable, binaries / name)
    built = subprocess.run(['cargo', 'build', '--locked', '--release', '-p', 'launcher',
                            '--bin', 'tilo-signal-provider', '--message-format=json-render-diagnostics'],
                           cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    providers = [item['executable'] for line in built.stdout.splitlines()
                 if (item := json.loads(line)).get('reason') == 'compiler-artifact'
                 and item.get('target', {}).get('name') == 'tilo-signal-provider'
                 and item.get('executable')]
    require(len(providers) == 1, 'Cargo omitted the owned provider')
    shutil.copy2(providers[0], binaries / 'tilo-signal-provider')
    strict = subprocess.run(['cargo', 'build', '--locked', '--release', '-p', 'launcher',
                             '--example', 'signal_strict_memory', '--message-format=json-render-diagnostics'],
                            cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    probes = [item['executable'] for line in strict.stdout.splitlines()
              if (item := json.loads(line)).get('reason') == 'compiler-artifact'
              and item.get('target', {}).get('name') == 'signal_strict_memory'
              and item.get('executable')]
    require(len(probes) == 1, 'Cargo omitted the strict runtime probe')
    shutil.copy2(probes[0], binaries / 'signal-strict-memory')
    release.native_payload(binaries, 'x86_64')
    payload = out / 'signal-provider'
    assets = out / 'assets'
    environment = dict(os.environ, TILO_GITHUB_ASSET_CACHE=str(assets))
    subprocess.run(['bash', 'os/dev/cache-github-assets.sh',
                    *[entry['name'] for entry in proof['pin']['assets'].values()],
                    proof['pin']['java_zero_source']['name']],
                   env=environment, cwd=ROOT, check=True)
    fixture.staging.stage(assets, payload, proof['pin'], 'x86_64')
    zero.build(out / 'java-zero-build', payload, assets, proof['pin'])
    source_tree = out / 'java-source'
    shutil.copytree(ROOT / 'os/signal-provider/java', source_tree)
    prepare.prepare(payload / 'signal/source.tar.gz', source_tree, proof['pin'])
    classes = payload / 'signal/tilo'
    classes.mkdir()
    subprocess.run(['javac', '--release', '25', '-classpath', str(payload / 'signal/lib/*'),
                    '-d', str(classes), *[str(path) for path in source_tree.rglob('*.java')]], check=True)
    tests = out / 'fixture-classes'
    tests.mkdir()
    classpath = str(classes) + ':' + str(payload / 'signal/lib/*')
    subprocess.run(['javac', '--release', '25', '-classpath', classpath, '-d', str(tests),
                    str(ROOT / 'tools/tests/SignalProviderIdentityFixture.java'),
                    str(ROOT / 'tools/tests/SignalProviderProxyFixture.java')], check=True)
    classpath = str(tests) + ':' + classpath
    subprocess.run(['java', '-classpath', classpath, 'SignalProviderIdentityFixture'], check=True)
    for name, proxy in (('empty', None), ('local', 'http://127.0.0.1:32123'),
                        ('invalid', 'http://example.invalid:32123')):
        environment = dict(os.environ)
        environment.pop('HTTP_PROXY', None)
        if proxy:
            environment['HTTP_PROXY'] = proxy
        subprocess.run(['java', '-classpath', classpath,
                        'org.asamk.signal.manager.config.SignalProviderProxyFixture', name],
                       env=environment, check=True)
    shutil.copytree(source_tree, payload / 'signal/tilo-source')
    # Package executable metadata must change the payload-bound native identity:
    # previously published bundles materialize this loader without execute bits.
    (payload / 'runtime-entrypoint.txt').write_text(RUNTIME_ENTRYPOINT + '\n')
    proof['compiled'] = {'bin': inventory(binaries), 'payload': inventory(payload)}
    (out / 'build.json').write_text(json.dumps(proof, sort_keys=True) + '\n')


def seal(out, source, sequence):
    expected = provenance(source)
    proof = json.loads((out / 'build.json').read_text())
    require({key: proof.get(key) for key in expected} == expected, 'compiled Signal provenance differs')
    require(proof['compiled'] == {'bin': inventory(out / 'bin'), 'payload': inventory(out / 'signal-provider')},
            'compiled Signal bytes changed')
    zero.verify(out / 'signal-provider', expected['pin'])
    require(re.fullmatch('[1-9][0-9]*', sequence), 'positive release sequence required')
    runtime_identity = os.environ.get('RUNTIME_BUNDLE_IDENTITY_FILE')
    app_identity = os.environ.get('TAPP_PUBLISHER_IDENTITY_FILE')
    require(runtime_identity and app_identity, 'existing runtime and app signing identities required; none is created')
    binary = out / 'bin/tilo-runtime-bundle'
    runtime_key = subprocess.check_output([str(binary), 'key', 'pubkey', '--identity', runtime_identity], text=True).strip()
    require(runtime_key == expected['runtime_publisher'], 'existing accepted runtime publisher differs')
    runtime = out / 'bundle'
    versions = out / 'bundles'
    version_file = out / 'runtime-version'
    subprocess.run([str(binary), 'build', '--release', RUNTIME.removeprefix('tilo-runtime/'),
                    '--arch', 'x86_64', '--from', '/', '--include', str(out / 'signal-provider'),
                    '--include', '/usr/lib64/libstdc++.so.6', '--include', '/usr/lib64/libz.so.1',
                    '--include', '/usr/lib64/libffi.so.8',
                    '--identity', runtime_identity, '--out-root', str(versions),
                    '--version-out', str(version_file)], check=True)
    bundle_version = version_file.read_text().strip()
    require(re.fullmatch(re.escape(RUNTIME.removeprefix('tilo-runtime/'))
                         + r'\+native\.[0-9a-f]{32}', bundle_version),
            'Signal runtime must bind its exact native build')
    shutil.move(versions / bundle_version, runtime)
    subprocess.run([str(binary), 'verify', str(runtime), '--require-signature',
                    '--require-publisher', runtime_key, '--require-arch', 'x86_64'], check=True)
    fixture.probe(out, prefix=(str(out / 'bin/signal-strict-memory'),))
    version = '0.1.' + sequence + '+g' + source
    subprocess.run(['bash', 'tools/apps/pack-runtime-package.sh', str(runtime), 'so.tilo.runtime.signal',
                    'Signal runtime', version, app_identity, str(out / 'bin'), str(out / 'runtime-package')],
                   cwd=ROOT, check=True)
    subprocess.run(['bash', 'tools/apps/pack-signal-provider.sh',
                    str(ROOT / 'crates/packaging/launcher/signal-provider.toml'),
                    str(out / 'bin/tilo-signal-provider'), app_identity, str(out / 'bin'),
                    str(out / 'provider-package'), str(runtime), version], cwd=ROOT, check=True)
    key = subprocess.check_output([str(binary), 'key', 'pubkey', '--identity', app_identity], text=True).strip()
    proof.update(package_version=version, app_publisher=key,
                 runtime_build='tilo-runtime/' + bundle_version)
    proof['packages'] = {name: inventory(out / name) for name in ('runtime-package', 'provider-package')}
    (out / 'release.json').write_text(json.dumps(proof, sort_keys=True) + '\n')
    verify(out, source, sequence, key)


def verify(out, source, sequence, trusted_key):
    proof = json.loads((out / 'release.json').read_text())
    expected = provenance(source)
    require({key: proof.get(key) for key in expected} == expected, 'Signal publication provenance differs')
    require(proof['package_version'] == '0.1.' + sequence + '+g' + source
            and proof['app_publisher'] == trusted_key, 'Signal package publisher/version differs')
    runtime_build = proof.get('runtime_build', '')
    require(re.fullmatch(re.escape(RUNTIME) + r'\+native\.[0-9a-f]{32}', runtime_build),
            'Signal runtime must bind its exact native build')
    cli = Path(os.environ.get('TILO_SIGNAL_VERIFIER_BIN_DIR', str(out / 'bin'))) / 'tilo-app-package'
    for name, package_id, kind in (('runtime-package', 'so.tilo.runtime.signal', 'runtime'),
                                   ('provider-package', 'so.tilo.signal-provider', 'service')):
        package = out / name
        require(inventory(package) == proof['packages'][name], 'Signal package inventory differs')
        tapps = list(package.glob('*.tapp'))
        require(len(tapps) == 1, 'one signed Signal package required')
        subprocess.run([str(cli), 'verify', '--trusted-key', trusted_key, str(tapps[0])], check=True)
        inspected = subprocess.check_output([str(cli), 'inspect', str(tapps[0])], text=True)
        manifest = tomllib.loads(inspected.split('--- manifest.toml ---\n', 1)[1].split('--- payload (', 1)[0])
        require(manifest['package']['id'] == package_id and manifest['package']['kind'] == kind
                and manifest['package']['version'] == proof['package_version'], 'Signal package declaration differs')
        require(manifest['package'].get('isa') == {'x86_64': 'v3'},
                'Signal package architecture/ISA differs')
        require(manifest['sandbox'].get('seccomp') == 'tilo-default'
                and manifest['sandbox'].get('landlock') == 'strict',
                'Signal package sandbox declaration differs')
        field = 'provides' if kind == 'runtime' else 'runtime'
        require(manifest['package'][field] == runtime_build and manifest['sandbox']['net']['mode'] == 'off',
                'Signal package runtime/network declaration differs')
        if kind in ('service', 'runtime'):
            # Packing serializes Entrypoint's splash defaults into the signed
            # manifest. Match that canonical declaration, including the absence
            # of alternate CLI/GUI binaries, rather than the source TOML shape.
            require(manifest.get('entrypoint') == {
                'bin': RUNTIME_ENTRYPOINT if kind == 'runtime' else 'bin/tilo-signal-provider',
                'type': 'cli',
                'splash-segments': [], 'splash-budget': 100,
            }, 'Signal executable declaration differs')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('plan', 'compile', 'native-proof', 'seal', 'verify'))
    parser.add_argument('--out', type=Path, default=ROOT / 'dist/signal-provider-release')
    parser.add_argument('--source')
    parser.add_argument('--sequence')
    parser.add_argument('--trusted-key')
    parser.add_argument('--source-commit')
    args = parser.parse_args()
    if args.operation == 'plan':
        enabled = mode(os.environ, release.head())
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write('signal_provider_only=' + str(enabled).lower() + '\n')
        return
    require(args.source and re.fullmatch('[0-9a-f]{40}', args.source), 'exact source required')
    require(not args.source_commit or args.operation == 'native-proof', 'snapshot proof cannot seal or publish')
    if args.operation == 'native-proof':
        require(args.source_commit and args.out.resolve().is_relative_to((ROOT / 'target').resolve()),
                'nonpublishing snapshot proof must use target output')
        source = release.Source(args.source, base64.b64decode(args.source_commit, validate=True))
        compile_provider(args.out.resolve(), source)
    elif args.operation == 'compile':
        compile_provider(args.out.resolve(), args.source)
    elif args.operation == 'seal':
        require(args.sequence, 'release sequence required')
        seal(args.out.resolve(), args.source, args.sequence)
    else:
        require(args.sequence and args.trusted_key and re.fullmatch('[0-9a-f]{64}', args.trusted_key),
                'release sequence and independently selected trusted key required')
        verify(args.out.resolve(), args.source, args.sequence, args.trusted_key)


if __name__ == '__main__':
    main()
