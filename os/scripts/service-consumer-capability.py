"""Bind optional service-session admission to actual signed receiving artifacts.

Candidate receiver qualification binds the native CEF qualifier to signed
artifacts. Admission remains unavailable until the actual sandbox credential
receiving route is qualified; codec/native-engine probes alone are insufficient.
"""
import json
import hashlib
import importlib.util
import os
import stat
import sys
from pathlib import Path
import platform
import re
import subprocess
import tomllib


def require(value, message):
    if not value:
        raise ValueError(message)


def hex_digest(value):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) is not None


def unique(items):
    value = {}
    for key, item in items:
        require(key not in value, 'duplicate service capability key')
        value[key] = item
    return value


def response(output, browser=False):
    value = json.loads(output, object_pairs_hook=unique)
    protocol = value.get('service_session_protocol') if isinstance(value, dict) else None
    require(type(protocol) is int and protocol in (1, 2), 'unsupported service protocol')
    expected = {'schema': 1, 'service_session_protocol': protocol, 'codec_verified': True}
    if browser:
        expected.update(custody_binding_verified=True, resource_action_verified=True)
    require(isinstance(value, dict) and set(value) == set(expected)
            and type(value.get('schema')) is int
            and type(value.get('service_session_protocol')) is int
            and all(type(value.get(key)) is bool for key in expected if key.endswith('_verified'))
            and value == expected, 'receiving service capability probe failed')
    return protocol


def native_origin_completion(output):
    value = json.loads(output, object_pairs_hook=unique)
    expected = {'schema': 1, 'service_session_protocol': 2,
                'cookie_persistence_verified': True,
                'origin_storage_persistence_verified': True}
    require(type(value) is dict and set(value) == set(expected)
            and type(value.get('schema')) is int
            and type(value.get('service_session_protocol')) is int
            and type(value.get('cookie_persistence_verified')) is bool
            and type(value.get('origin_storage_persistence_verified')) is bool
            and value == expected, 'native CEF origin persistence qualification did not complete')


def native_qualify(command, env):
    result = subprocess.run(command, env=env, capture_output=True, check=True, timeout=300)
    require(len(result.stdout) <= 4096 and len(result.stderr) <= 4096,
            'native service qualification exceeded output bound')
    return result.stdout


def selected(root):
    registry = root / 'usr/share/tilo-apps/registry.toml'
    if not registry.exists():
        return None
    entries = tomllib.loads(registry.read_text()).get('entry', [])
    browser = [entry for entry in entries if entry.get('id') == 'so.tilo.browser']
    require(len(browser) <= 1, 'ambiguous receiving Browser package')
    if not browser:
        return None
    row = browser[0]
    require(hex_digest(row.get('package_id_hex')) and hex_digest(row.get('publisher_key_hex')),
            'Browser package has no signed identity')
    # Canonical installed-store layout, not a caller-supplied executable path.
    payload = '/usr/share/tilo-apps/pkgs/' + row['package_id_hex']
    metadata = payload + '.pkg/manifest.toml'
    require(row.get('payload_root') == payload and row.get('manifest_path') == metadata,
            'Browser package does not use the canonical image store')
    manifest_path = root / metadata.removeprefix('/')
    manifest = tomllib.loads(manifest_path.read_text())
    package = manifest.get('package', {})
    require(package.get('id') == 'so.tilo.browser', 'receiving manifest is not Browser')
    runtimes = package.get('optional-runtimes', [])
    engines = [value.removeprefix('tilo-runtime/') for value in runtimes
               if isinstance(value, str) and value.startswith('tilo-runtime/') and value.endswith('-chromium')]
    require(len(engines) == 1, 'Browser must name one exact receiving CEF runtime')
    version = engines[0]
    require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,255}', version), 'unsafe receiving CEF version')
    bundle = root / 'usr/lib/tilo-runtime' / version
    if not bundle.exists():
        return None
    require(manifest.get('entrypoint', {}).get('bin') == 'bin/browser',
            'receiving Browser executable differs from canonical entrypoint')
    return row, package, root / payload.removeprefix('/') / 'bin/browser', version, bundle


RECEIVING_ENTRY = '/usr/libexec/tilo-installer-receive-fixture/receive.py'
RECEIVING_DEPENDENCIES = tuple(
    '/usr/libexec/tilo-installer-receive-fixture/' + name + '.py'
    for name in ('receive', 'semantic', 'firstuse', 'delivery', 'namespace',
                 'packages', 'apps', 'cloud', 'service_sessions', 'service_contract', 'service_processes')) + tuple(
    '/usr/libexec/tilo-installer-source-fixture/' + name + '.py'
    for name in ('source', 'owned_gpt', 'bounded_native', 'flatpak_material', 'source_sessions'))
# Schema 2 is the complete measured Python receiving closure. Schema 1
# candidate Browser/CEF proofs are never sufficient for service admission.
SERVICE_SCHEMA = 2
ROUTE_KEYS = {'receiving_shell_path', 'receiving_shell_sha256',
              'receiving_entry_sha256', 'receiving_dependencies',
              'receiving_python_path', 'receiving_python_sha256',
              'receiving_route_qualification_sha256'}


def route_artifacts(root, shell_path, primitives):
    require(shell_path in ('/usr/bin/shell', '/usr/bin/tilo-shell'),
            'receiving Shell is not a canonical packaged receiver')
    digest, _interpreter, regular = primitives
    shell = root / shell_path.removeprefix('/')
    require(regular(shell, 256 * 1024 * 1024).st_mode & 0o111,
            'receiving Shell is not executable')
    dependencies = {}
    for relative in RECEIVING_DEPENDENCIES:
        path = root / relative.removeprefix('/')
        regular(path, 1024 * 1024)
        dependencies[relative] = digest(path, 1024 * 1024)
    # Python's fixed installed alias may target its versioned ELF, but may not
    # redirect outside the admitted image. The target is measured independently.
    python_alias = root / 'usr/bin/python3'
    python = python_alias.resolve(strict=True)
    require(python.parent == root / 'usr/bin'
            and re.fullmatch(r'python3(?:\.[0-9]+)?', python.name) is not None,
            'receiving Python escapes the canonical image interpreter identity')
    require(regular(python, 256 * 1024 * 1024).st_mode & 0o111,
            'receiving Python is not executable')
    facts = {'receiving_shell_sha256': digest(shell, 256 * 1024 * 1024),
             'receiving_entry_sha256': dependencies[RECEIVING_ENTRY],
             'receiving_dependencies': dependencies,
             'receiving_python_path': '/' + str(python.relative_to(root)),
             'receiving_python_sha256': digest(python, 256 * 1024 * 1024)}
    encoded = json.dumps(facts, sort_keys=True, separators=(',', ':')).encode()
    facts['receiving_route_qualification_sha256'] = hashlib.sha256(
        b'tilo-installed-service-receiver-python-v1\0' + encoded).hexdigest()
    return facts


def produce(root, runner, primitives, sdk, qualify=False, evidence=None,
            qualifier=native_qualify, route_qualifier=None):
    # A saved receipt attests prior native qualification. Read-only check
    # revalidates its artifacts without repeating TLS/enrollment or rewriting.
    if evidence is not None:
        require(type(evidence) is dict and ROUTE_KEYS <= set(evidence),
                'saved service proof lacks qualified sandbox credential receiving route')
        require(type(evidence.get('schema')) is int and evidence['schema'] == SERVICE_SCHEMA,
                'unsupported measured receiving closure schema')
        candidate_evidence = {key: value for key, value in evidence.items() if key not in ROUTE_KEYS}
        candidate_evidence['schema'] = 1
        candidate = qualify_receivers(root, runner, primitives, sdk,
                                      evidence=candidate_evidence, qualifier=qualifier)
        facts = route_artifacts(root, evidence['receiving_shell_path'], primitives)
        proof = dict(candidate, schema=SERVICE_SCHEMA,
                     receiving_shell_path=evidence['receiving_shell_path'], **facts)
        require(proof == evidence, 'installed receiving route differs from qualified consumers')
        return proof
    if qualify:
        # Cloud grants do not imply an installed, qualified service-session
        # receiver. Only explicitly provisioned qualification tools enable
        # this optional proof; a saved proof above may never disappear.
        tools = (root / RECEIVING_ENTRY.removeprefix('/'),
                 root / 'usr/libexec/tilo-service-session-qualification.py')
        present = [path.exists() or path.is_symlink() for path in tools]
        require(all(present) or not any(present), 'incomplete optional service qualification')
        if not any(present):
            return None
    candidate = qualify_receivers(root, runner, primitives, sdk,
                                  qualify=qualify, qualifier=qualifier)
    if candidate is None:
        return None
    shells = [name for name in ('/usr/bin/shell', '/usr/bin/tilo-shell')
              if (root / name.removeprefix('/')).exists()]
    require(len(shells) == 1, 'expected one canonical receiving Shell')
    proof = qualify_receiving_route(root, sdk, candidate, primitives, shells[0],
                                    route_qualifier or native_route_qualify)
    # Route qualification executes the actual Shell; revalidate the signed
    # Browser/CEF/SDK and exact native qualifier again after it returns.
    require(qualify_receivers(root, runner, primitives, sdk, evidence=candidate) == candidate,
            'receiving consumers changed during route qualification')
    return proof


def qualify_receivers(root, runner, primitives, sdk, qualify=False, evidence=None,
                      qualifier=native_qualify):
    """Candidate artifact qualification; not sufficient for service admission."""
    # No claim is made on an old account-only image. A saved claim may never
    # disappear merely because one of its receiving artifacts went missing.
    if not qualify and evidence is None:
        return None
    receiving = selected(root)
    if receiving is None:
        require(evidence is None, 'saved service consumer is no longer installed')
        return None
    row, package, browser, version, bundle = receiving
    require(package.get('runtime') == 'tilo-runtime/' + sdk['runtime_version'],
            'Browser does not receive the selected SDK runtime')
    digest, interpreter, regular = primitives
    publisher = (root / 'usr/share/tilo/browser-runtime-publisher.hex').read_text().strip()
    require(hex_digest(publisher), 'invalid receiving CEF publisher')
    verify = [str(root / 'usr/bin/tilo-runtime-bundle'), 'verify', str(bundle),
              '--require-publisher', publisher, '--require-signature',
              '--require-arch', platform.machine()]
    env = {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'}
    runner(verify, env)
    manifest = tomllib.loads((bundle / 'bundle.toml').read_text())
    require(manifest.get('version') == version and hex_digest(manifest.get('root_hash')),
            'verified receiving CEF identity differs')
    names = [item['path'] for item in manifest['files']]
    require(len(set(names)) == len(names) and 'lib/chromium/tilo-chromium-host' in names,
            'CEF host is absent from signed receiving inventory')
    host = bundle / 'lib/chromium/tilo-chromium-host'
    script = root / 'usr/libexec/tilo-service-session-qualification.py'
    for path in (browser, host, script):
        require(regular(path, 256 * 1024 * 1024).st_mode & 0o111,
                'receiving service artifact is not executable')
    # The canonical verifier chooses this exact installed image row, never a
    # same-id package from an importing user's writable store.
    package_env = dict(env, TILO_APPS_DATA_DIR=str(root / 'usr/share/tilo-apps'))
    verify_browser = [str(root / 'usr/bin/tilo-install'), 'verify', 'so.tilo.browser']
    runner(verify_browser, package_env)
    primary = root / 'usr/lib/tilo-runtime' / sdk['runtime_version']
    probe = primary / 'bin/tilo-runtime-account-capability'
    loader = primary / interpreter(probe)
    loaded = [str(loader), '--inhibit-cache', '--library-path', str(primary / 'lib')]
    try:
        protocol = response(runner(loaded + [str(probe), '--service-session-capability'], env))
        browser_protocol = response(runner(loaded + [str(browser), '--service-session-capability'], env), browser=True)
        require(protocol == browser_protocol, 'receiving service protocols differ')
    except subprocess.CalledProcessError:
        # Old receivers may retain protected account grants without asserting
        # service support. A previously qualified receiver cannot downgrade.
        require(qualify and evidence is None, 'saved service receiver no longer supports its protocol')
        return None
    proof = {'schema': 1, 'protocol': protocol, 'sdk_runtime_root_hash': sdk['runtime_root_hash'],
             'browser_package_hash': row['package_id_hex'], 'browser_publisher': row['publisher_key_hex'],
             'browser_binary_sha256': digest(browser, 256 * 1024 * 1024),
             'cef_runtime_version': version, 'cef_runtime_root_hash': manifest['root_hash'],
             'cef_runtime_publisher': publisher, 'cef_host_sha256': digest(host, 256 * 1024 * 1024),
             'qualification_script_sha256': digest(script, 1024 * 1024)}
    if protocol == 2:
        proof['origin_storage_qualification_sha256'] = proof['qualification_script_sha256']
    if qualify:
        command = ['/usr/bin/python3', str(script), str(host)]
        if protocol == 2:
            command.append('--capability-receipt')
            native_origin_completion(qualifier(command, env))
        else:
            require(qualifier(command, env) == b'selected CEF session import: PASS\n',
                    'native CEF Set/Flush/restart qualification did not complete')
    else:
        require(type(evidence) is dict and set(evidence) == set(proof)
                and type(evidence.get('schema')) is int and type(evidence.get('protocol')) is int
                and evidence == proof, 'installed service capability differs from qualified consumers')
    # Neither active probe nor native qualification may replace the receiving
    # artifacts before the image records their capability.
    runner(verify, env)
    runner(verify_browser, package_env)
    share = root / 'usr/share/tilo'
    markers = [share / name for name in ('account-consumer-runtime-version',
                                        'account-consumer-runtime-publisher.hex')]
    present = [path.exists() for path in markers]
    require(all(present) or not any(present), 'incomplete account runtime selection')
    if all(present):
        require(markers[0].read_text().strip() == sdk['runtime_version'],
                'service proof differs from selected account runtime')
    sdk_publisher = (markers[1] if all(present) else share / 'runtime-bundle-publisher.hex').read_text().strip()
    require(hex_digest(sdk_publisher), 'invalid selected SDK publisher')
    runner([str(root / 'usr/bin/tilo-runtime-bundle'), 'verify', str(primary),
            '--require-publisher', sdk_publisher, '--require-signature',
            '--require-arch', platform.machine()], env)
    require(tomllib.loads((primary / 'bundle.toml').read_text()).get('root_hash') == sdk['runtime_root_hash']
            and digest(probe, 64 * 1024 * 1024) == sdk['runtime_probe_sha256']
            and digest(primary / sdk['runtime_library'], 2 * 1024 * 1024 * 1024) == sdk['runtime_library_sha256'],
            'receiving SDK changed during qualification')
    require(digest(browser, 256 * 1024 * 1024) == proof['browser_binary_sha256']
            and digest(host, 256 * 1024 * 1024) == proof['cef_host_sha256']
            and digest(script, 1024 * 1024) == proof['qualification_script_sha256'],
            'receiving service artifacts changed during qualification')
    return proof


def native_route_qualify(command, env, request):
    # Fixed image-controlled code checks the existing owned candidate, evidence
    # and outer namespace witness before starting the canonical coordinator.
    path = Path('/usr/libexec/tilo-installer-receive-fixture/service_contract.py')
    require(json.loads(request, object_pairs_hook=unique).get('root') == '/',
            'service qualification requires the actual owned receiving image')
    require(os.getuid() == os.geteuid() == 0,
            'service route qualification requires the canonical owned receiving coordinator')
    for edge in (path, *path.parents):
        info = edge.lstat()
        require(info.st_uid == 0 and not info.st_mode & 0o022
                and not stat.S_ISLNK(info.st_mode), 'service bridge is not image controlled')
    saved = list(sys.path)
    try:
        sys.path.insert(0, str(path.parent))
        spec = importlib.util.spec_from_file_location('owned_service_contract', path)
        bridge = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(bridge)
        return bridge.owned_bridge(command, env, request)
    finally:
        sys.path[:] = saved


def qualify_receiving_route(root, sdk, candidate, primitives, shell_path,
                            qualifier=native_route_qualify):
    """Qualify actual admitted sandbox route; never called by admission yet."""
    require(shell_path in ('/usr/bin/shell', '/usr/bin/tilo-shell'),
            'receiving Shell is not a canonical packaged receiver')
    require(root.is_absolute() and root.resolve() == root,
            'receiving root is not canonical')
    facts = route_artifacts(root, shell_path, primitives)
    driver = root / RECEIVING_ENTRY.removeprefix('/')
    require(candidate['sdk_runtime_root_hash'] == sdk['runtime_root_hash']
            and candidate['protocol'] in (1, 2), 'route candidate differs from selected SDK')
    request = {'schema': 2, 'root': str(root), 'shell_path': shell_path,
               'sdk_runtime_version': sdk['runtime_version'],
               'sdk_runtime_root_hash': sdk['runtime_root_hash'],
               'browser_package_hash': candidate['browser_package_hash'],
               'browser_publisher': candidate['browser_publisher'],
               'cef_runtime_version': candidate['cef_runtime_version'],
               'cef_runtime_root_hash': candidate['cef_runtime_root_hash'],
               'cef_runtime_publisher': candidate['cef_runtime_publisher'],
               'required_protocol': candidate['protocol'], **facts}
    encoded = json.dumps(request, sort_keys=True, separators=(',', ':')).encode()
    require(len(encoded) <= 4096, 'receiving route request exceeds bound')
    output = qualifier([str(root / facts['receiving_python_path'].removeprefix('/')), '-I',
                        str(driver), '--service-session-qualification'],
                       {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'LC_ALL': 'C'}, encoded)
    require(len(output) <= 4096, 'receiving route result exceeds bound')
    value = json.loads(output, object_pairs_hook=unique)
    expected = {'schema': 2, 'service_session_protocol': candidate['protocol'],
                'receiving_route_qualification_sha256': facts['receiving_route_qualification_sha256'],
                'private_list_verified': True, 'private_resolve_verified': True,
                'isolated_profile_verified': True, 'revocation_verified': True,
                'no_vault_mount_verified': True,
                'origin_storage_persisted': candidate['protocol'] == 2,
                'chat_destination_navigation_verified': True}
    require(type(value) is dict and set(value) == set(expected)
            and type(value.get('schema')) is int
            and type(value.get('service_session_protocol')) is int
            and all(type(value.get(key)) is bool for key in expected
                    if key not in ('schema', 'service_session_protocol', 'receiving_route_qualification_sha256'))
            and value == expected, 'actual sandbox receiving route did not complete')
    require(route_artifacts(root, shell_path, primitives) == facts,
            'receiving Shell or fixed Python closure changed during qualification')
    return dict(candidate, schema=SERVICE_SCHEMA, receiving_shell_path=shell_path, **facts)
