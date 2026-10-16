#!/usr/bin/env python3
"""Assemble the existing apps+runtime component in private final-path mounts.

No OS/kernel build and no host /usr writes. The caller owns the normal native
build admission. The result is only a staged component; tilo-vmd still owns
source authority, import, activation and the slot lifecycle.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import tomllib


def run(argv, *, env=None, capture=False):
    return subprocess.run(argv, check=True, env=env,
                          stdout=subprocess.PIPE if capture else None,
                          text=capture)


def applications(repo):
    rows = []
    for path in sorted((repo / 'apps').glob('*/tilo-app.toml')):
        manifest = tomllib.loads(path.read_text())
        cargo = tomllib.loads(path.with_name('Cargo.toml').read_text())
        release = manifest['package'].get('runtime', '')
        if not re.fullmatch(r'tilo-runtime/[0-9A-Za-z.-]+', release):
            raise ValueError('apps+runtime requires an authored toolkit release')
        entry = manifest.get('entrypoint', {}).get('bin', '')
        if not entry.startswith('bin/') or '/' in entry[4:]:
            raise ValueError('apps+runtime requires a native binary entrypoint')
        rows.append((cargo['package']['name'], entry[4:], release))
    if not rows or len({row[2] for row in rows}) != 1:
        raise ValueError('apps+runtime must share one authored toolkit release')
    return rows


def selected_applications(repo, app):
    rows = applications(repo)
    if app is None:
        return rows
    if not re.fullmatch(r'[A-Za-z0-9_-]+', app):
        raise ValueError('invalid selected application directory')
    manifest = repo / 'apps' / app / 'tilo-app.toml'
    if not manifest.is_file():
        raise ValueError('selected app is outside the canonical runtime graph')
    cargo = tomllib.loads(manifest.with_name('Cargo.toml').read_text())
    return [row for row in rows if row[0] == cargo['package']['name']]


def private_ancestors(path, outputs):
    """Copy directory structure only along outputs; host siblings stay readonly."""
    argv = ['--dir', str(path)]
    for child in sorted(path.iterdir()):
        if child in outputs:
            continue
        if any(output.is_relative_to(child) for output in outputs):
            argv += private_ancestors(child, outputs)
        elif child.is_symlink():
            argv += ['--symlink', os.readlink(child), str(child)]
        else:
            argv += ['--ro-bind', str(child), str(child)]
    return argv


def namespace_command(repo, stage, tools, profile, *, packaging=False, app=None, compiler=None,
                      source_inventory=None):
    """Final-path binds preserve registry paths, with only owned outputs writable."""
    tree = stage / 'tree'
    runtime = tree / 'usr/lib/tilo-runtime'
    store = tree / 'usr/share/tilo-apps'
    pins = tree / 'usr/share/tilo'
    for path in (runtime, store, pins, stage / 'home', stage / 'work'):
        path.mkdir(parents=True, exist_ok=True)
    for name in ('runtime-bundle-publisher.hex', 'app-publisher.hex',
                 'browser-runtime-publisher.hex'):
        if not packaging:
            (pins / name).write_text('')
            (pins / name).chmod(0o644)
    if not packaging:
        shutil.copyfile(repo / 'os/registry-runtime-publisher.hex',
                        pins / 'registry-runtime-publisher.hex')
        (pins / 'registry-runtime-publisher.hex').chmod(0o644)
        (stage / 'work/trusted-publishers.txt').write_text('')
    argv = ['bwrap', '--die-with-parent', '--new-session', '--unshare-all', '--uid', '0', '--gid', '0',
            '--ro-bind', '/', '/', '--dev', '/dev', '--proc', '/proc',
            '--tmpfs', '/tmp', '--tmpfs', '/usr', '--tmpfs', '/etc']
    outputs = {Path('/usr/lib/tilo-runtime'), Path('/usr/share/tilo-apps'),
               Path('/usr/share/tilo'), Path('/etc/tilo')}
    argv += private_ancestors(Path('/usr'), outputs)
    argv += private_ancestors(Path('/etc'), outputs)
    argv += ['--bind', str(stage), str(stage),
            '--bind', str(runtime), '/usr/lib/tilo-runtime',
            '--bind', str(store), '/usr/share/tilo-apps',
            '--dir', '/usr/share/tilo', '--dir', '/etc/tilo',
            '--bind', str(stage / 'work/trusted-publishers.txt'),
            '/etc/tilo/runtime-bundle-trusted-publishers.txt']
    for pin in sorted(pins.iterdir()):
        readonly = packaging and pin.name != 'app-publisher.hex'
        argv += ['--ro-bind' if readonly else '--bind', str(pin), '/usr/share/tilo/' + pin.name]
    for variable in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME',
                     'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
        argv += ['--setenv', variable, str(stage / 'home')]
    argv += ['--setenv', 'TMPDIR', '/tmp',
             '--unsetenv', 'LD_PRELOAD', '--unsetenv', 'LD_LIBRARY_PATH',
             '--chdir', str(repo), 'python3', str(Path(__file__).resolve()),
             '--inside', '--repo', str(repo), '--target-dir', str(tools),
             '--profile', profile, '--stage', str(stage)]
    if source_inventory is not None:
        argv += ['--source-inventory', source_inventory]
    if app is not None:
        argv += ['--app', app]
    if compiler is not None:
        argv += ['--compiler-interface-out', str(stage / 'work/compiler-bundles'),
                 '--compiler-capture', str(compiler)]
    if packaging:
        argv += ['--packaging']
    return argv


def profile_directory(profile):
    return 'debug' if profile == 'dev' else profile


def runtime_dependencies():
    """os/scripts/collect-runtime-dependencies.py, which the release bundles share."""
    path = Path(__file__).resolve().parents[1] / 'scripts/collect-runtime-dependencies.py'
    spec = importlib.util.spec_from_file_location('collect_runtime_dependencies', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def runtime_engines(directory):
    """The engine libraries build-thin-apps.sh built beside the toolkit.

    Signed into the SDK whichever apps are selected, like the toolkit itself:
    each is loaded only by its app, but the runtime id covers it."""
    listing = directory / 'runtime-engines'
    if not listing.is_file():
        raise ValueError('build-thin-apps.sh left no runtime engine list beside the toolkit')
    names = listing.read_text().split()
    if any(not re.fullmatch(r'lib[a-z0-9_]+\.so', name) for name in names):
        raise ValueError('a listed runtime engine is not a built library beside the toolkit')
    engines = [directory / name for name in names]
    if any(not path.is_file() for path in engines):
        raise ValueError('a listed runtime engine is not a built library beside the toolkit')
    return engines


def native_dependencies(objects, dependency_env):
    """Collect the native closure of every ELF before signing its SDK, with the
    libraries the toolkit loads on first use (GLib, GStreamer; #4020)."""
    collector = runtime_dependencies()
    library_dirs = [Path(d) for d in dependency_env.get('LD_LIBRARY_PATH', '').split(':') if d]
    cache = {}
    lazy = list(dict.fromkeys(collector.locate(soname, library_dirs, cache)
                              for binary in objects for soname in collector.lazy_needed(binary)))
    paths = set(lazy)
    for binary in [*objects, *lazy]:
        report = run(['ldd', str(binary)], env=dependency_env, capture=True).stdout
        if 'not found' in report:
            raise ValueError(f'{binary} has unresolved native dependencies')
        paths.update(map(Path, re.findall(r'(?:=>\s*)?(/[^\s()]+)', report)))
    return sorted(paths)


def prepare_runtime_fonts(repo, directory):
    """Ship the reviewed standalone catalog inside the signed SDK payload."""
    fonts = directory / 'fonts'
    run(['python3', str(repo / 'tools/font-catalog.py'), 'prepare',
         str(repo / 'crates/sdk/ui-core/assets/fonts'), str(fonts),
         '--scope', 'product'])
    return fonts


def publish_runtime_fonts(repo, fonts, catalogs):
    """Expose the same verified bytes to image processes outside app sandboxes."""
    identity = hashlib.sha256((fonts / 'manifest.toml').read_bytes()).hexdigest()
    catalog = catalogs / identity
    run(['python3', str(repo / 'tools/font-catalog.py'), 'stage',
         str(fonts), str(catalog), '--scope', 'product'])
    return catalog


def source_membership(repo, destination, digest):
    """Seal the exact source membership inside the signed toolkit payload."""
    if not re.fullmatch(r'[0-9a-f]{64}', digest):
        raise ValueError('invalid source inventory digest')
    ids = [tomllib.loads(path.read_text())['package']['id']
           for path in sorted((repo / 'apps').glob('*/tilo-app.toml'))]
    if not ids or len(ids) > 512 or len(set(ids)) != len(ids):
        raise ValueError('source system application membership is empty, oversized or duplicated')
    destination.write_text(json.dumps({'schema': 1, 'source_inventory': digest,
                                      'apps': sorted(ids)}, sort_keys=True) + '\n')
    return destination


def assemble_inside(args):
    repo, target, stage = args.repo, args.target_dir, args.stage
    rows = applications(repo)
    binaries = [target / profile_directory(args.profile) / row[1] for row in rows]
    if any(not binary.is_file() for binary in binaries):
        raise ValueError('a required cohort binary is absent; refusing a partial store')
    toolkit = target / profile_directory(args.profile) / 'libruntime_dylib.so'
    libdir = Path(run(['rustc', '--print', 'target-libdir'], capture=True).stdout.strip())
    std = sorted(libdir.glob('libstd-*.so'))
    if len(std) != 1 or not toolkit.is_file():
        raise ValueError('the exact graph toolkit or Rust standard library is missing')
    engines = runtime_engines(toolkit.parent)
    dependency_env = dict(os.environ, LD_LIBRARY_PATH=f'{toolkit.parent}:{libdir}')
    closure = native_dependencies([toolkit, *engines, *binaries], dependency_env)
    fonts = prepare_runtime_fonts(repo, stage / 'work')
    publish_runtime_fonts(repo, fonts, stage / 'tree/usr/share/tilo/font-catalogs')
    includes = list(dict.fromkeys([toolkit, *engines, std[0], *closure, fonts]))
    if args.source_inventory is not None:
        includes.append(source_membership(repo, stage / 'work/system-app-cohort.json',
                                          args.source_inventory))
    bundle = target / 'release/tilo-runtime-bundle'
    identity = stage / 'work/runtime-identity.json'
    version_file = stage / 'work/runtime-version'
    run([str(bundle), 'key', 'generate', '--out', str(identity)])
    try:
        command = [str(bundle), 'build', '--release', rows[0][2].split('/')[1],
                   '--arch', platform.machine(), '--from', '/',
                   '--identity', str(identity), '--out-root', '/usr/lib/tilo-runtime',
                   '--version-out', str(version_file)]
        for path in includes:
            command += ['--include', str(path)]
        run(command)
        publisher = run([str(bundle), 'key', 'pubkey', '--identity', str(identity)],
                        capture=True).stdout.strip()
        if not re.fullmatch(r'[0-9a-f]{64}', publisher):
            raise ValueError('runtime publisher is malformed')
        (stage / 'tree/usr/share/tilo/runtime-bundle-publisher.hex').write_text(publisher + '\n')
        (stage / 'work/trusted-publishers.txt').write_text(publisher + '\n')
        if args.compiler_interface_out is not None:
            # Seal the runtime first; only then bind a separate developer bundle.
            # It is outside the component tree and never reaches end users.
            runtime = Path('/usr/lib/tilo-runtime') / version_file.read_text().strip()
            manifest = tomllib.loads((runtime / 'bundle.toml').read_text())
            output = run([str(target / 'release/tilo-app'), '--json', 'sdk', 'export',
                          '--capture', str(args.compiler_capture),
                          '--sdk-bundle', str(runtime), '--sdk-root', manifest['root_hash'],
                          '--sdk-publisher', publisher, '--identity', str(identity),
                          '--bundle-tool', str(bundle), '--output', str(args.compiler_interface_out)],
                         capture=True).stdout
            (stage / 'work/compiler-receipt.json').write_text(output)

    finally:
        identity.unlink(missing_ok=True)
    version = version_file.read_text().strip()
    runtime = Path('/usr/lib/tilo-runtime') / version
    run([str(bundle), 'verify', str(runtime), '--require-publisher', publisher,
         '--require-signature', '--require-arch', platform.machine()])
    run(['bash', str(repo / 'os/scripts/verify-runtime-bundle-closure.sh'),
         str(runtime), *map(str, binaries)])
    # Packing happens in a fresh namespace with immutable generated pins.


def engine_pins(repo):
    """The optional runtimes the cohort's apps pin by exact version (the
    Browser's engine): they ship with the apps or the cohort is refused."""
    pins = set()
    for path in sorted((repo / 'apps').glob('*/tilo-app.toml')):
        manifest = tomllib.loads(path.read_text())
        pins.update(manifest['package'].get('optional-runtimes', []))
    for pin in pins:
        if not re.fullmatch(r'tilo-runtime/[0-9A-Za-z][0-9A-Za-z._+-]*', pin):
            raise ValueError(f'invalid optional runtime {pin!r}')
    return sorted(pins)


def host_runtime_roots():
    """Where this host already keeps runtime bundles, read only."""
    roots = []
    if os.environ.get('TILO_RUNTIME_BUNDLE_DIR'):
        roots.append(Path(os.environ['TILO_RUNTIME_BUNDLE_DIR']))
    data = os.environ.get('XDG_DATA_HOME') or str(Path.home() / '.local/share')
    roots += [Path(data) / 'tilo-runtime', Path('/usr/lib/tilo-runtime')]
    return roots


def fetch_runtime(reference, destination, app_cli):
    """Fetch `reference` through the workspace's own verified install (the
    `app` CLI built with the other tools) into `destination`, never into this
    person's runtime directory."""
    destination.mkdir(parents=True, exist_ok=True)
    run([str(app_cli), 'runtime', 'install', reference],
        env=dict(os.environ, TILO_RUNTIME_BUNDLE_DIR=str(destination)))


def stage_engine_runtimes(repo, stage, bundle, roots=None, fetch=None):
    """Ship every engine the cohort's apps pin inside the layer, at the exact
    version, and trust the publisher that signed it. A userspace swap that
    replaced the Browser without its engine would leave it saying "not
    installed"; so the engine comes from this host's bundles, else the
    registry, and the cohort is refused when neither has it."""
    publishers = set()
    runtime_root = stage / 'tree/usr/lib/tilo-runtime'
    for reference in engine_pins(repo):
        version = reference.split('/', 1)[1]
        source = next((root / version for root in (host_runtime_roots() if roots is None else roots)
                       if (root / version / 'bundle.toml').is_file()), None)
        if source is None:
            fetched = stage / 'work/engine-fetch'
            if fetch is None:
                fetch_runtime(reference, fetched, bundle.parent / 'app')
            else:
                fetch(reference, fetched)
            source = fetched / version
        if not (source / 'bundle.toml').is_file():
            raise ValueError(f'the engine {reference} the apps pin is neither on this host nor in the registry')
        report = run([str(bundle), 'verify', str(source), '--require-signature'], capture=True).stdout
        found = re.search(r'publisher: ed25519:([0-9a-f]{64})', report)
        if not found:
            raise ValueError(f'{reference} is not signed by a publisher')
        publishers.add(found.group(1))
        shutil.copytree(source, runtime_root / version, symlinks=True, dirs_exist_ok=True)
    if len(publishers) > 1:
        raise ValueError('the cohort pins engines from more than one publisher')
    if publishers:
        (stage / 'tree/usr/share/tilo/browser-runtime-publisher.hex').write_text(publishers.pop() + '\n')


def package_inside(args):
    repo, target, stage = args.repo, args.target_dir, args.stage
    rows = selected_applications(repo, args.app)
    version = (stage / 'work/runtime-version').read_text().strip()
    bundle = target / 'release/tilo-runtime-bundle'
    env = dict(os.environ, REPO=str(repo), STORE='/usr/share/tilo-apps',
               BIN_DIR=str(target / profile_directory(args.profile)),
               PKG_BIN=str(target / 'release/tilo-app-package'),
               INSTALL_BIN=str(target / 'release/tilo-install'),
               BUNDLE_BIN=str(bundle), SHIM_BIN=str(target / 'release/tilo-cli-shim'),
               PUBKEY_OUT=str(stage / 'tree/usr/share/tilo/app-publisher.hex'),
               RUNTIME_ID='tilo-runtime/' + version, REQUIRE_MIN=str(len(rows)))
    if args.app is not None:
        env.update(APP_DIR=args.app, TAPP_OUT=str(stage / 'work/selected.tapp'),
                   STAGE=str(stage / 'work/selected-pack'))
    else:
        env['COHORT_TAPP_DIR'] = '/usr/share/tilo-apps/cohort'
    run(['bash', str(repo / 'os/scripts/pack-apps.sh')], env=env)



def prepare(args):
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    cargo = os.environ.get('CARGO', 'cargo')
    tools_env = dict(os.environ, CARGO_PROFILE_RELEASE_LTO='off',
                     CARGO_PROFILE_RELEASE_CODEGEN_UNITS='16')
    tools = [cargo, 'build', '--locked', '--release', '-p', 'runtime-bundle',
             '-p', 'tilo-app-package-cli', '-p', 'install', '-p', 'cli-shim',
             # `app runtime install`: the registry fallback for a pinned engine.
             '-p', 'app-cli']
    if args.compiler_interface_out is not None:
        tools += ['-p', 'tilo-app-cli']
    run(tools, env=tools_env)
    env = dict(os.environ, REPO=str(repo), TARGET_DIR=str(target),
               PROFILE=args.profile, APP_FEATURES=args.features)
    for variable in ('EXCLUDE_PACKAGES', 'INCLUDE_PACKAGES', 'BUILD_PACKAGES', 'EXTRA_CARGO_ARGS'):
        env.pop(variable, None)
    # The SDK is shared by the complete collection, including native libraries
    # linked only by an app. Warm unchanged targets are reused by Cargo; a
    # selected update packages only that app but derives the same complete SDK.
    if args.compiler_interface_out is not None:
        env['SDK_COMPILER_UNITS'] = str(args.compiler_capture.parent / 'compiler-units.jsonl')
    run(['bash', str(repo / 'os/scripts/build-thin-apps.sh')], env=env)
    if args.compiler_interface_out is not None:
        run([str(target / 'release/tilo-app'), 'sdk', 'capture',
             '--units', str(args.compiler_capture.parent / 'compiler-units.jsonl'),
             '--output', str(args.compiler_capture)])

    for _, binary, _ in selected_applications(repo, args.app):
        if not (target / profile_directory(args.profile) / binary).is_file():
            raise ValueError('full cohort build did not produce all applications')


def assemble(args):
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    if args.dest is not None and args.dest.exists() and any(args.dest.iterdir()):
        raise ValueError('component destination must be empty')
    target.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='app-runtime-', dir=target) as directory:
        stage = Path(directory)
        args.compiler_capture = (stage / 'compiler-capture'
                                 if args.compiler_interface_out is not None else None)
        prepare(args)
        run(namespace_command(repo, stage, target, args.profile, app=args.app,
                              compiler=args.compiler_capture, source_inventory=args.source_inventory))
        if args.app is None:
            stage_engine_runtimes(repo, stage, target / 'release/tilo-runtime-bundle')
        run(namespace_command(repo, stage, target, args.profile, packaging=True, app=args.app))
        if args.app is not None:
            shutil.copyfile(stage / 'work/selected.tapp', args.tapp_out)
            return
        for path in (stage / 'tree').rglob('*'):
            if path.is_dir() and not path.is_symlink():
                path.chmod(0o755)
        # The ephemeral keys/home/work are outside tree and never exported.
        shutil.copytree(stage / 'tree', args.dest, dirs_exist_ok=True)
        if args.compiler_interface_out is not None:
            shutil.copytree(stage / 'work/compiler-bundles', args.compiler_interface_out)
            receipt = json.loads((stage / 'work/compiler-receipt.json').read_text())
            receipt['result']['bundle'] = str(args.compiler_interface_out / Path(receipt['result']['bundle']).name)
            (args.compiler_interface_out / 'receipt.json').write_text(json.dumps(receipt, sort_keys=True) + '\n')



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--profile', required=True)
    parser.add_argument('--features')
    parser.add_argument('--source-inventory', help='verified complete source inventory digest')
    parser.add_argument('--app')
    parser.add_argument('--tapp-out', type=Path)
    parser.add_argument('--dest', type=Path)
    parser.add_argument('--compiler-interface-out', type=Path, help='separate signed developer SDK; never part of the userspace layer')
    parser.add_argument('--compiler-capture', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--stage', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--packaging', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_-]+', args.profile):
        parser.error('invalid build profile')
    if args.features is None:
        components = tomllib.loads((args.repo / 'os/components.toml').read_text())['component']
        cohort = next(row for row in components if row['name'] == 'apps+runtime')
        if args.profile != cohort.get('profile'):
            parser.error('selected app must use the canonical cohort profile')
        args.features = ','.join(cohort.get('features', []))
    if args.app is not None:
        selected_applications(args.repo, args.app)
    if args.compiler_interface_out is not None and not args.inside:
        args.compiler_interface_out = args.compiler_interface_out.resolve()
        if args.app is not None or args.compiler_interface_out.exists():
            parser.error('compiler interface requires full SDK assembly and a new output directory')
        if args.dest is not None and args.compiler_interface_out.is_relative_to(args.dest.resolve()):
            parser.error('developer compiler interface must stay outside the userspace layer')
    if args.inside:
        if args.stage is None:
            parser.error('private assembly needs its owned stage')
        if args.packaging:
            package_inside(args)
        else:
            assemble_inside(args)
    else:
        if args.app is not None:
            if args.tapp_out is None or args.dest is not None:
                parser.error('selected app needs only --tapp-out')
        elif args.dest is None or args.tapp_out is not None:
            parser.error('assembly needs only a component destination')
        assemble(args)


if __name__ == '__main__':
    main()
