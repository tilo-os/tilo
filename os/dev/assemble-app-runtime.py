#!/usr/bin/env python3
"""Assemble the existing apps+runtime component in private final-path mounts.

No OS/kernel build and no host /usr writes. The caller owns the normal native
build admission. The result is only a staged component; the device's steps still own
source authority, import, activation and the account lifecycle.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import tomllib


# Where a Tilo image keeps the verified font artifacts the runtime bundle's
# catalog is prepared from (tools/font-catalog.py's IMAGE_SEED).
FONT_SEED = Path('/usr/share/tilo/font-artifacts')


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


def image_plan(repo):
    """The app set this assembly ships, planned from the sources as the image plans one.

    The image's compile writes the plan of its `image` collection
    (tools/apps/app_release.py): every native app, and the tilolang packages under
    `apps/tl` that ship beside the native app they replace. The packing below reads
    the same plan, so a swap installs exactly the apps the image installs, and an app
    the collection does not name yet is not installed by either."""
    plan = json.loads(run([sys.executable, '-I', str(repo / 'tools/apps/app_release.py'),
                           '--collection', 'image'], capture=True).stdout)
    tilolang_apps(plan)
    return plan


def tilolang_apps(plan):
    """The tilolang packages a plan names, checked for the shape the packer reads.

    Each is built and installed by the release's own steps
    (os/scripts/pack-apps.sh through tools/apps/tilolang_packages.sh), so this only
    refuses a plan whose entries are outside `apps/tl` or name a runtime the
    launcher could not resolve."""
    apps = plan.get('tilolang', [])
    for app in apps:
        if not re.fullmatch(r'tl-[A-Za-z0-9_-]+', app.get('app', '')):
            raise ValueError(f'invalid tilolang package name {app.get("app")!r}')
        if not re.fullmatch(r'apps/tl/[A-Za-z0-9_.-]+', app.get('dir', '')):
            raise ValueError(f'{app["app"]} is outside apps/tl')
        if not re.fullmatch(r'[A-Za-z0-9_.-]+', app.get('id', '')):
            raise ValueError(f'{app["app"]} names no app id')
        if not re.fullmatch(r'tilolang/[0-9A-Za-z][0-9A-Za-z._+-]*', app.get('runtime', '')):
            raise ValueError(f'{app["app"]} names no tilolang runtime')
    names = [app['app'] for app in apps]
    if len(set(names)) != len(names):
        raise ValueError('the plan names a tilolang package twice')
    return apps


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
                      source_inventory=None, cohort=None, app_plan=None):
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
    # The image's font seed, read only: `/usr/share/tilo` is replaced here for
    # the pins above, and the runtime bundle's font catalog is prepared from
    # this seed. Without it a swap on a Tilo image has to fetch every font into
    # a namespace that has no network.
    if FONT_SEED.is_dir():
        argv += ['--ro-bind', str(FONT_SEED), str(FONT_SEED)]
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
    if app_plan is not None:
        argv += ['--app-plan', str(app_plan)]
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


def lto_enabled(value):
    return value is True or (isinstance(value, str)
                             and value.strip().lower() not in ('', '0', 'false', 'off', 'no', 'none'))


def profile_links_with_lto(repo, profile, environ):
    """Whether `profile` links its binaries with LTO.

    Cargo's own setting decides: `CARGO_PROFILE_<PROFILE>_LTO` when the caller
    sets it, else the `[profile.<name>]` chain in Cargo.toml, where `inherits`
    hands the parent's value down.
    """
    key = 'CARGO_PROFILE_{}_LTO'.format(profile.upper().replace('-', '_'))
    if key in environ:
        return lto_enabled(environ[key])
    profiles = tomllib.loads((Path(repo) / 'Cargo.toml').read_text()).get('profile', {})
    seen = set()
    while profile in profiles and profile not in seen:
        seen.add(profile)
        settings = profiles[profile]
        if 'lto' in settings:
            return lto_enabled(settings['lto'])
        profile = settings.get('inherits')
    return False


def packaging_budget(repo, profile, environ):
    """`TAPP_BUDGET` for packing this build's apps.

    The per-app size budgets are measured on the thin-LTO release binaries the
    image ships. A build linked without LTO is larger for the same code, so its
    over-budget packages are reported instead of refusing the cohort, the way
    the image treats its own non-LTO builds. A caller that packs binaries the
    budgets measure names them: `TAPP_BUDGET=enforce`, or the LTO setting that
    made them so.
    """
    if 'TAPP_BUDGET' in environ:
        return environ['TAPP_BUDGET']
    return 'enforce' if profile_links_with_lto(repo, profile, environ) else 'report'


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


def bake_tilolang_runtime(repo, target, profile, identity, runtime_root):
    """Bake the tilolang runtime bundle beside the Tilo runtime, as the image bakes it.

    The bundle is the two binaries alone (`--binaries-only` in
    tools/apps/tilolang_packages.sh, the release's and the image's own packing): the
    launcher runs them on the Tilo runtime their libraries come from. It is signed
    by the identity that signed the Tilo runtime, so one publisher pin trusts both,
    and it lands at `/<version>`, where the launcher resolves the
    `tilolang/<version>` every package names. Returns the version baked."""
    packer = shlex.quote(str(repo / 'tools/apps/tilolang_packages.sh'))
    script = '\n'.join([
        'set -euo pipefail',
        f'. {packer}',
        f'version="$(tilolang_runtime_version {shlex.quote(str(repo))})"',
        'tilolang_bake_runtime '
        f'{shlex.quote(str(target / "release/tilo-runtime-bundle"))} '
        f'{shlex.quote(str(target / profile_directory(profile) / "tilolang-app"))} '
        f'{shlex.quote(str(target / "release/tilolang-process"))} '
        f'{shlex.quote(str(identity))} "$1/$version" "$version"',
        'printf "%s\\n" "$version"',
        '',
    ])
    return run(['bash', '-c', script, 'bake-tilolang-runtime', str(runtime_root)],
               capture=True).stdout.strip()


def assemble_inside(args):
    repo, target, stage = args.repo, args.target_dir, args.stage
    entries = named_applications(repo, args.cohort, args.app)
    tilolang = tilolang_apps(args.plan) if args.plan is not None else []
    binaries = [target / profile_directory(args.profile) / entry['binary'] for entry in entries]
    binaries += [target / 'release/tilolang-process'] if tilolang else []  # runs on it as well
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
    publish_runtime_fonts(repo, fonts, stage / 'tree/usr/share/tilo/font-catalogs')
    includes = list(dict.fromkeys([toolkit, *engines, std[0], *closure, fonts]))
    model_runtime = target / 'app-models/app-model-runtime'
    if model_entries(repo, entries) and (model_runtime / 'runtime.json').is_file():
        includes.append(model_runtime)
    if args.source_inventory is not None:
        includes.append(source_membership(
            stage / 'work/system-app-cohort.json', args.source_inventory,
            [entry['id'] for entry in entries] + [app['id'] for app in tilolang]))
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
        # The tilolang runtime the cohort's packages execute, beside the Tilo
        # runtime and under the same publisher: `tilo-install` refuses a package
        # whose runtime is absent, so it is in place before packing runs.
        if tilolang:
            tilolang_version = bake_tilolang_runtime(
                repo, target, args.profile, identity, Path('/usr/lib/tilo-runtime'))
            run([str(bundle), 'verify', str(Path('/usr/lib/tilo-runtime') / tilolang_version),
                 '--require-publisher', publisher, '--require-signature',
                 '--require-arch', platform.machine()])
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
    tilolang = tilolang_apps(args.plan) if args.plan is not None and args.app is None else []
    version = (stage / 'work/runtime-version').read_text().strip()
    bundle = target / 'release/tilo-runtime-bundle'
    # The tilolang packages are packed by the release's own steps
    # (os/scripts/pack-apps.sh through tools/apps/tilolang_packages.sh), from the
    # plan the image's packing reads: this names the plan and the compiler that
    # builds each package, never a second packer.
    cohort = [entry['dir'] for entry in entries] + [app['app'] for app in tilolang]
    env = dict(os.environ, REPO=str(repo), STORE='/usr/share/tilo-apps',
               BIN_DIR=str(target / profile_directory(args.profile)),
               PKG_BIN=str(target / 'release/tilo-app-package'),
               INSTALL_BIN=str(target / 'release/tilo-install'),
               BUNDLE_BIN=str(bundle), SHIM_BIN=str(target / 'release/tilo-cli-shim'),
               PUBKEY_OUT=str(stage / 'tree/usr/share/tilo/app-publisher.hex'),
               RUNTIME_ID='tilo-runtime/' + version, REQUIRE_MIN=str(len(cohort)),
               COHORT_APPS=' '.join(cohort),
               TAPP_BUDGET=packaging_budget(repo, args.profile, os.environ))
    if args.app_plan is not None:
        env['APP_PLAN'] = str(args.app_plan)
    if tilolang:
        env['TILOLANG_BIN'] = str(target / 'release/tilolang')
    # A complete cohort carries each app once, as its installed tree and signed
    # record; the device's steps rebuild the archive it installs from those.
    if args.app is not None:
        env.update(TAPP_OUT=str(stage / 'work/selected.tapp'),
                   STAGE=str(stage / 'work/selected-pack'))
    run(['bash', str(repo / 'os/scripts/pack-apps.sh')], env=env)


def prepare_tools(args, tilolang=False):
    """The tools this assembly runs itself, including the registry fallback for
    a pinned engine: locating one is what decides the cohort."""
    cargo = os.environ.get('CARGO', 'cargo')
    tools_env = dict(os.environ, CARGO_PROFILE_RELEASE_LTO='off',
                     CARGO_PROFILE_RELEASE_CODEGEN_UNITS='16')
    tools = [cargo, 'build', '--locked', '--release', '-p', 'runtime-bundle',
             '-p', 'tilo-app-package-cli', '-p', 'install', '-p', 'cli-shim',
             # `app runtime install`: the registry fallback for a pinned engine.
             '-p', 'app-cli']
    if tilolang:
        # `tilolang build`, and the host the tilolang runtime bundle ships: the
        # image builds both when its app set names a tilolang package. The
        # bundle takes executables up to 64 MiB, and a release link without LTO
        # keeps every symbol: the host ships stripped, as the thin apps do.
        tools += ['-p', 'tilolang-cli', '-p', 'tilolang-process',
                  '--config',
                  'profile.release.package."tilolang-process".strip="symbols"']
    if args.compiler_interface_out is not None:
        tools += ['-p', 'tilo-app-cli']
    run(tools, env=tools_env)


def application_build_env(args, entries):
    """What os/scripts/build-thin-apps.sh reads to build this cohort."""
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
    return env


def prepare_applications(args, entries):
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    env = application_build_env(args, entries)
    run(['bash', str(repo / 'os/scripts/build-thin-apps.sh')], env=env)
    if args.compiler_interface_out is not None:
        run([str(target / 'release/tilo-app'), 'sdk', 'capture',
             '--units', str(args.compiler_capture.parent / 'compiler-units.jsonl'),
             '--output', str(args.compiler_capture)])

    for entry in entries:
        if not (target / profile_directory(args.profile) / entry['binary']).is_file():
            raise ValueError('full cohort build did not produce all applications')


def prepare_tilolang_applications(args, entries):
    """Build the tilolang runtime's app binary beside the cohort's apps.

    It is the binary every tilolang package's entrypoint executes
    (`bin/tilolang-app`, a thin app). build-thin-apps.sh selects a
    `tilo-runtime.toml` binary only when BUILD_PACKAGES names it, so this is a
    build of its own, as the image makes it, and the cohort's own build keeps
    selecting every app."""
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    env = dict(application_build_env(args, entries), BUILD_PACKAGES='tilolang-app')
    # The developer SDK's unit inventory is the cohort's apps, already captured.
    env.pop('SDK_COMPILER_UNITS', None)
    run(['bash', str(repo / 'os/scripts/build-thin-apps.sh')], env=env)
    binary = target / profile_directory(args.profile) / 'tilolang-app'
    if not binary.is_file():
        raise ValueError('the cohort build did not produce the tilolang runtime binary')


def model_entries(repo, entries):
    """The applications that keep a model: a guest crate beside the app
    (apps/tasks -> apps/tasks-model-wasm), whose module the pack requires."""
    return [entry for entry in entries
            if (repo / 'apps' / (entry['dir'] + '-model-wasm') / 'Cargo.toml').is_file()]


def prepare_models(args, entries):
    """Build every app's model module and the model runtime they link
    against, as the image's app-models step does: the pack refuses an app
    that declares a model without its module, and the runtime bundle carries
    the model runtime the modules name."""
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    if not model_entries(repo, entries):
        return None
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    run(['bash', str(repo / 'tools/ci/app-models.sh')], env=env)
    runtime = target / 'app-models/app-model-runtime'
    if not (runtime / 'runtime.json').is_file():
        raise ValueError('tools/ci/app-models.sh wrote no model runtime')
    return runtime


def assemble(args):
    repo, target = args.repo.resolve(), args.target_dir.resolve()
    if args.dest is not None and args.dest.exists() and any(args.dest.iterdir()):
        raise ValueError('component destination must be empty')
    target.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='app-runtime-', dir=target) as directory:
        stage = Path(directory)
        args.compiler_capture = (stage / 'compiler-capture'
                                 if args.compiler_interface_out is not None else None)
        # The plan the image's packing reads, written where both namespaces can
        # read it. A single selected app ships alone, so it plans nothing.
        args.plan = None
        args.app_plan = None
        if args.app is None:
            args.plan = image_plan(repo)
            (stage / 'work').mkdir(parents=True, exist_ok=True)
            args.app_plan = stage / 'work/app-plan.json'
            args.app_plan.write_text(json.dumps(args.plan, indent=2) + '\n')
        tilolang = bool(tilolang_apps(args.plan)) if args.plan is not None else False
        prepare_tools(args, tilolang=tilolang)
        fetch_runtime_fonts(repo, target)
        sources, unavailable = engine_sources(repo, stage, target / 'release/app')
        entries, left_out = cohort_entries(repo, sources, args.app)
        report_left_out(left_out, unavailable)
        prepare_applications(args, entries)
        if tilolang:
            prepare_tilolang_applications(args, entries)
        prepare_models(args, entries)
        cohort = ','.join(entry['dir'] for entry in entries)
        run(namespace_command(repo, stage, target, args.profile, app=args.app,
                              compiler=args.compiler_capture, app_plan=args.app_plan,
                              source_inventory=args.source_inventory, cohort=cohort))
        if args.app is None:
            stage_engine_runtimes(stage, target / 'release/tilo-runtime-bundle', sources)
        run(namespace_command(repo, stage, target, args.profile, packaging=True,
                              app=args.app, cohort=cohort, app_plan=args.app_plan))
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
    parser.add_argument('--app-plan', type=Path,
                        help='the app set this assembly ships, as the image plans it')
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
        if args.app_plan is not None:
            args.plan = json.loads(args.app_plan.read_text())
        else:
            args.plan = None
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
