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
import sys
import tempfile
import tomllib


def run(argv, *, env=None, capture=False):
    return subprocess.run(argv, check=True, env=env,
                          stdout=subprocess.PIPE if capture else None,
                          text=capture)


def report(message):
    """Say what the cohort is, on the builder's log."""
    print(f'apps+runtime: {message}', file=sys.stderr, flush=True)


def required_runtimes(package):
    """The further runtime builds an authored app requires, by exact version.

    The manifest key is `optional-runtimes`; the app needs every build it
    names to run at all (the Browser's engine)."""
    pins = package.get('optional-runtimes', [])
    for pin in pins:
        if not re.fullmatch(r'tilo-runtime/[0-9A-Za-z][0-9A-Za-z._+-]*', pin):
            raise ValueError(f'invalid required runtime {pin!r}')
    if len(set(pins)) != len(pins):
        raise ValueError(f'a required runtime of {package.get("id")!r} is listed twice')
    return list(pins)


def application_entries(repo):
    """Every authored application in the runtime graph: its directory, manifest
    identity, cargo package, entrypoint binary, toolkit release and the further
    runtime builds it requires."""
    entries = []
    for path in sorted((repo / 'apps').glob('*/tilo-app.toml')):
        manifest = tomllib.loads(path.read_text())
        package = manifest['package']
        cargo = tomllib.loads(path.with_name('Cargo.toml').read_text())
        release = package.get('runtime', '')
        if not re.fullmatch(r'tilo-runtime/[0-9A-Za-z.-]+', release):
            raise ValueError('apps+runtime requires an authored toolkit release')
        entry = manifest.get('entrypoint', {}).get('bin', '')
        if not entry.startswith('bin/') or '/' in entry[4:]:
            raise ValueError('apps+runtime requires a native binary entrypoint')
        entries.append({'dir': path.parent.name, 'id': package.get('id', ''),
                        'name': package.get('name', ''),
                        'package': cargo['package']['name'],
                        'binary': entry[4:], 'release': release,
                        'pins': required_runtimes(package)})
    if not entries or len({entry['release'] for entry in entries}) != 1:
        raise ValueError('apps+runtime must share one authored toolkit release')
    return entries


def applications(repo):
    """Every application in the runtime graph, as (cargo package, binary, release)."""
    return [(entry['package'], entry['binary'], entry['release'])
            for entry in application_entries(repo)]


def named_applications(repo, cohort, app=None):
    """The applications one chosen assembly is made of: the app directories the
    caller decided on, or the single app it selected, in source order."""
    entries = application_entries(repo)
    if app is not None:
        if not re.fullmatch(r'[A-Za-z0-9_-]+', app):
            raise ValueError('invalid selected application directory')
        if not (repo / 'apps' / app / 'tilo-app.toml').is_file():
            raise ValueError('selected app is outside the canonical runtime graph')
        entries = [entry for entry in entries if entry['dir'] == app]
    if cohort is None:
        return entries
    names = [name for name in cohort.split(',') if name]
    if not names:
        raise ValueError('the cohort names no authored application')
    if len(set(names)) != len(names):
        raise ValueError('the cohort names an application twice')
    known = {entry['dir'] for entry in entries}
    if any(name not in known for name in names):
        raise ValueError('the cohort names an application this source does not author')
    return [entry for entry in entries if entry['dir'] in set(names)]


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
                      source_inventory=None, cohort=None):
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
    if cohort is not None:
        argv += ['--cohort', cohort]
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


def fetch_runtime_fonts(repo, target):
    """Fetch the reviewed fonts into the artifact cache before the namespaces, which have no
    network (`--unshare-all`), a home of their own and an empty /tmp: the font cache they would
    look in is empty, or hidden, so on a builder without a warm cache of its own (a t node, whose
    home is /tmp) their prepare could only download, and failed. Without a cache the caller names,
    the build directory holds it, which the namespaces see; entries are verified when read."""
    if os.environ.get('TILO_FONT_CATALOG_ROOT'):
        return
    if not os.environ.get('TILO_FONT_ASSET_CACHE') and not os.environ.get('TILO_TEST_CACHE_DIR'):
        os.environ['TILO_FONT_ASSET_CACHE'] = str(target / 'font-artifacts')
    run(['python3', str(repo / 'tools/font-catalog.py'), 'root',
         str(repo / 'crates/sdk/ui-core/assets/fonts'), '--scope', 'product'], capture=True)


def publish_runtime_fonts(repo, fonts, catalogs):
    """Expose the same verified bytes to image processes outside app sandboxes."""
    identity = hashlib.sha256((fonts / 'manifest.toml').read_bytes()).hexdigest()
    catalog = catalogs / identity
    run(['python3', str(repo / 'tools/font-catalog.py'), 'stage',
         str(fonts), str(catalog), '--scope', 'product'])
    return catalog


def source_membership(destination, digest, apps):
    """Seal the exact cohort membership inside the signed toolkit payload.

    It is what the daemon admits the layer by: the installed store must be
    exactly these applications, so an app the assembly left out is neither
    claimed nor silently expected."""
    if not re.fullmatch(r'[0-9a-f]{64}', digest):
        raise ValueError('invalid source inventory digest')
    ids = list(apps)
    if not ids or len(ids) > 512 or len(set(ids)) != len(ids):
        raise ValueError('source system application membership is empty, oversized or duplicated')
    destination.write_text(json.dumps({'schema': 1, 'source_inventory': digest,
                                      'apps': sorted(ids)}, sort_keys=True) + '\n')
    return destination


def assemble_inside(args):
    repo, target, stage = args.repo, args.target_dir, args.stage
    entries = named_applications(repo, args.cohort, args.app)
    binaries = [target / profile_directory(args.profile) / entry['binary']
                for entry in entries]
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
    bundle = target / 'release/tilo-runtime-bundle'
    # Every process that sets text from this catalog reads its faces from
    # the record instead of parsing each font; the copies below carry it.
    run([str(bundle), 'font-index', str(fonts)])
    publish_runtime_fonts(repo, fonts, stage / 'tree/usr/share/tilo/font-catalogs')
    includes = list(dict.fromkeys([toolkit, *engines, std[0], *closure, fonts]))
    if args.source_inventory is not None:
        includes.append(source_membership(stage / 'work/system-app-cohort.json',
                                          args.source_inventory,
                                          [entry['id'] for entry in entries]))
    identity = stage / 'work/runtime-identity.json'
    version_file = stage / 'work/runtime-version'
    run([str(bundle), 'key', 'generate', '--out', str(identity)])
    try:
        command = [str(bundle), 'build', '--release', entries[0]['release'].split('/')[1],
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
    """The further runtimes the source's apps require by exact version (the
    Browser's engine): each ships with its app or the app leaves the cohort."""
    return sorted({pin for entry in application_entries(repo) for pin in entry['pins']})


def container_host_root():
    """The host's root as a toolbox/podman container sees it, else None."""
    return Path('/run/host') if Path('/run/.containerenv').is_file() else None


def host_runtime_roots(environ=os.environ, host_root=None):
    """Where this host already keeps runtime bundles, read only.

    Built in a toolbox (as `tdev` does), `/usr` is the container's, which has
    no engine; the host's own installed engines stay at `/run/host/usr`."""
    roots = []
    if environ.get('TILO_RUNTIME_BUNDLE_DIR'):
        roots.append(Path(environ['TILO_RUNTIME_BUNDLE_DIR']))
    data = environ.get('XDG_DATA_HOME') or str(Path.home() / '.local/share')
    roots += [Path(data) / 'tilo-runtime', Path('/usr/lib/tilo-runtime')]
    if host_root is not None:
        roots.append(host_root / 'usr/lib/tilo-runtime')
    return roots


def fetch_runtime(reference, destination, app_cli):
    """Fetch `reference` through the workspace's own verified install (the
    `app` CLI built with the other tools) into `destination`, never into this
    person's runtime directory. Returns why no registry offered it, or None
    when one did."""
    destination.mkdir(parents=True, exist_ok=True)
    result = subprocess.run([str(app_cli), 'runtime', 'install', reference],
                            env=dict(os.environ, TILO_RUNTIME_BUNDLE_DIR=str(destination)),
                            capture_output=True, text=True)
    if result.returncode == 0:
        return None
    said = [line.strip() for line in (result.stderr or result.stdout).splitlines()
            if line.strip()]
    return said[-1] if said else f'{app_cli} exited {result.returncode}'


def engine_sources(repo, stage, app_cli, roots=None, fetch=None):
    """Locate the bundle behind every engine pin: this host's installed copy,
    else the registry through the `app` CLI. What neither offers is reported
    as unavailable rather than refused here: the apps that pin it then leave
    the cohort (see `cohort_entries`)."""
    if roots is None:
        roots = host_runtime_roots(host_root=container_host_root())
    sources, unavailable = {}, {}
    for reference in engine_pins(repo):
        version = reference.split('/', 1)[1]
        source = next((root / version for root in roots
                       if (root / version / 'bundle.toml').is_file()), None)
        if source is None:
            fetched = stage / 'work/engine-fetch'
            reason = (fetch or (lambda reference, destination: fetch_runtime(
                reference, destination, app_cli)))(reference, fetched)
            source = fetched / version
            if reason is not None or not (source / 'bundle.toml').is_file():
                unavailable[reference] = reason or 'no registry offered a bundle for it'
                continue
        sources[reference] = source
    return sources, unavailable


def cohort_entries(repo, sources, app=None):
    """The applications this assembly builds and ships, and the ones it cannot.

    An app runs only with every runtime build it pins, so the two ship
    together or not at all: an app whose engine neither this host nor the
    registry offers is left out, and says so, because a cohort that keeps
    every other app is worth more than one app that would install and then
    refuse to start. A selected app (`app`) ships alone as a package the
    ordinary installer fetches its engine for, so it is never left out.

    What is required still refuses the assembly: a malformed pin, an engine
    that does not verify, a signed version that disagrees with its directory,
    engines from two publishers, a toolkit release that is not one — and, at
    packing time, `check-shipped-runtimes.sh` for the installed cohort."""
    entries = application_entries(repo)
    if app is not None:
        return named_applications(repo, None, app), []
    missing = {entry['dir']: [pin for pin in entry['pins'] if pin not in sources]
               for entry in entries}
    cohort = [entry for entry in entries if not missing[entry['dir']]]
    left_out = [(entry, missing[entry['dir']]) for entry in entries if missing[entry['dir']]]
    if not cohort:
        raise ValueError('no application in the cohort has the runtime it requires')
    return cohort, left_out


def report_left_out(left_out, unavailable):
    for entry, pins in left_out:
        reasons = '; '.join(f'{pin}: {unavailable[pin]}' for pin in pins)
        report(f'left out apps/{entry["dir"]} ({entry["name"]}): an app ships with the '
               f'runtimes it requires or not at all, and {reasons}')


def stage_engine_runtimes(stage, bundle, sources):
    """Ship every located engine inside the layer, at the exact version, and
    trust the publisher that signed it. A userspace swap that installed the
    Browser without its engine would leave it saying "not installed", so the
    cohort's apps and their engines always travel together."""
    publishers = set()
    runtime_root = stage / 'tree/usr/lib/tilo-runtime'
    for reference, source in sources.items():
        version = reference.split('/', 1)[1]
        report_text = run([str(bundle), 'verify', str(source), '--require-signature',
                           '--require-arch', platform.machine()], capture=True).stdout
        manifest = tomllib.loads((source / 'bundle.toml').read_text())
        if manifest.get('version') != version:
            raise ValueError(f'{reference} directory contains a different signed engine version')
        found = re.search(r'publisher: ed25519:([0-9a-f]{64})', report_text)
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
    entries = named_applications(repo, args.cohort, args.app)
    version = (stage / 'work/runtime-version').read_text().strip()
    bundle = target / 'release/tilo-runtime-bundle'
    env = dict(os.environ, REPO=str(repo), STORE='/usr/share/tilo-apps',
               BIN_DIR=str(target / profile_directory(args.profile)),
               PKG_BIN=str(target / 'release/tilo-app-package'),
               INSTALL_BIN=str(target / 'release/tilo-install'),
               BUNDLE_BIN=str(bundle), SHIM_BIN=str(target / 'release/tilo-cli-shim'),
               PUBKEY_OUT=str(stage / 'tree/usr/share/tilo/app-publisher.hex'),
               RUNTIME_ID='tilo-runtime/' + version, REQUIRE_MIN=str(len(entries)),
               COHORT_APPS=' '.join(entry['dir'] for entry in entries))
    # A complete cohort carries each app once, as its installed tree and signed
    # record; tilo-vmd rebuilds the archive it installs from those.
    if args.app is not None:
        env.update(TAPP_OUT=str(stage / 'work/selected.tapp'),
                   STAGE=str(stage / 'work/selected-pack'))
    run(['bash', str(repo / 'os/scripts/pack-apps.sh')], env=env)


def prepare_tools(args):
    """The tools this assembly runs itself, including the registry fallback for
    a pinned engine: locating one is what decides the cohort."""
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


def prepare_applications(args, entries):
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    env = dict(os.environ, REPO=str(repo), TARGET_DIR=str(target),
               PROFILE=args.profile, APP_FEATURES=args.features)
    for variable in ('INCLUDE_PACKAGES', 'BUILD_PACKAGES', 'EXTRA_CARGO_ARGS'):
        env.pop(variable, None)
    # An app the cohort leaves out is not built with it: build-thin-apps.sh
    # leaves its features out too, and its binary then never exists to be
    # packaged.
    env['EXCLUDE_PACKAGES'] = ' '.join(
        entry['package'] for entry in application_entries(repo) if entry not in entries)
    if args.compiler_interface_out is not None:
        env['SDK_COMPILER_UNITS'] = str(args.compiler_capture.parent / 'compiler-units.jsonl')
    run(['bash', str(repo / 'os/scripts/build-thin-apps.sh')], env=env)
    if args.compiler_interface_out is not None:
        run([str(target / 'release/tilo-app'), 'sdk', 'capture',
             '--units', str(args.compiler_capture.parent / 'compiler-units.jsonl'),
             '--output', str(args.compiler_capture)])

    for entry in entries:
        if not (target / profile_directory(args.profile) / entry['binary']).is_file():
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
        prepare_tools(args)
        fetch_runtime_fonts(repo, target)
        sources, unavailable = engine_sources(repo, stage, target / 'release/app')
        entries, left_out = cohort_entries(repo, sources, args.app)
        report_left_out(left_out, unavailable)
        prepare_applications(args, entries)
        cohort = ','.join(entry['dir'] for entry in entries)
        run(namespace_command(repo, stage, target, args.profile, app=args.app,
                              compiler=args.compiler_capture,
                              source_inventory=args.source_inventory, cohort=cohort))
        if args.app is None:
            stage_engine_runtimes(stage, target / 'release/tilo-runtime-bundle', sources)
        run(namespace_command(repo, stage, target, args.profile, packaging=True,
                              app=args.app, cohort=cohort))
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
    parser.add_argument('--cohort', help='the app directories this assembly ships')
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
        named_applications(args.repo, None, args.app)
    if args.compiler_interface_out is not None and not args.inside:
        args.compiler_interface_out = args.compiler_interface_out.resolve()
        if args.app is not None or args.compiler_interface_out.exists():
            parser.error('compiler interface requires full SDK assembly and a new output directory')
        if args.dest is not None and args.compiler_interface_out.is_relative_to(args.dest.resolve()):
            parser.error('developer compiler interface must stay outside the userspace layer')
    if args.inside:
        if args.stage is None:
            parser.error('private assembly needs its owned stage')
        if args.cohort is None:
            parser.error('private assembly needs the cohort it ships')
        if args.packaging:
            package_inside(args)
        else:
            assemble_inside(args)
    else:
        if args.cohort is not None:
            parser.error('the assembly decides the cohort its caller asked for')
        if args.app is not None:
            if args.tapp_out is None or args.dest is not None:
                parser.error('selected app needs only --tapp-out')
        elif args.dest is None or args.tapp_out is not None:
            parser.error('assembly needs only a component destination')
        assemble(args)


if __name__ == '__main__':
    main()
