"""Bounded, read-only runtime evidence. This is not a relocation adapter."""
from __future__ import annotations

from collections import deque
from dataclasses import dataclass
import hashlib
from pathlib import Path
import re
import stat
import subprocess
import time

from collection_native import MACHO, run, system_library


@dataclass(frozen=True)
class AuditConfig:
    binary: Path
    prefix: Path
    roots: tuple[Path, ...]
    architecture: str = 'arm64'
    minimum_macos: str = '13.0'
    max_images: int = 512
    seconds: int = 120


def version(value):
    if not re.fullmatch(r'\d+(?:\.\d+){0,2}', value):
        raise ValueError(f'invalid macOS version: {value}')
    return tuple((list(map(int, value.split('.'))) + [0, 0])[:3])


def remaining(deadline):
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise ValueError('audit time bound reached')
    return seconds


def regular_file(path, limit, roots=()):
    resolved = path.resolve(strict=True)
    if roots and not any(resolved.is_relative_to(root) for root in roots):
        raise ValueError(f'metadata escapes explicit roots: {path}')
    details = resolved.stat()
    if not stat.S_ISREG(details.st_mode):
        raise ValueError(f'not a regular file: {path}')
    if details.st_size > limit:
        raise ValueError(f'file exceeds {limit} byte audit limit: {path}')
    return resolved


def bounded_hash(path, limit, deadline):
    value, total = hashlib.sha256(), 0
    with path.open('rb') as stream:
        while True:
            remaining(deadline)
            chunk = stream.read(1024**2)
            if not chunk:
                return value.hexdigest()
            total += len(chunk)
            if total > limit:
                raise ValueError('file grew beyond audit byte limit')
            value.update(chunk)


def directory_entries(path, maximum, deadline):
    entries = []
    for entry in path.iterdir():
        remaining(deadline)
        if len(entries) >= maximum:
            raise ValueError('directory entry bound reached')
        entries.append(entry)
    return sorted(entries)


def image_info(path, architecture, timeout):
    deadline = time.monotonic() + timeout
    path = regular_file(path, 1024**3)
    with path.open('rb') as stream:
        if stream.read(4) not in MACHO:
            raise ValueError('not a Mach-O image')
    arches = run(['/usr/bin/lipo', '-archs', path], timeout=remaining(deadline)).split()
    expected = ['arm64', 'x86_64'] if architecture == 'universal' else [architecture]
    if set(arches) != set(expected):
        raise ValueError(f'architecture mismatch: {arches}, expected {expected}')
    slices = {}
    for arch in expected:
        text = run(['/usr/bin/otool', '-arch', arch, '-l', path], timeout=remaining(deadline))
        dependencies, rpaths, install_id = [], [], None
        for block in re.split(r'Load command \d+\n', text)[1:]:
            cmd = re.search(r'\bcmd (LC_\w+)', block)
            name = re.search(r'\bname (.+?) \(offset', block)
            if cmd and name and cmd[1] in {'LC_LOAD_DYLIB', 'LC_LOAD_WEAK_DYLIB', 'LC_REEXPORT_DYLIB',
                                            'LC_LOAD_UPWARD_DYLIB', 'LC_LAZY_LOAD_DYLIB'}:
                dependencies.append({'load': name[1], 'kind': cmd[1]})
            elif cmd and name and cmd[1] == 'LC_ID_DYLIB':
                install_id = name[1]
            if cmd and cmd[1] == 'LC_RPATH':
                match = re.search(r'\bpath (.+?) \(offset', block)
                if not match:
                    raise ValueError('malformed LC_RPATH')
                rpaths.append(match[1])
        minimum = re.search(r'\bminos ([0-9.]+)', text) or re.search(
            r'cmd LC_VERSION_MIN_MACOSX\n\s+cmdsize \d+\n\s+version ([0-9.]+)', text)
        platform = re.search(r'\bplatform (\S+)', text)
        if not minimum or (platform and platform[1] not in {'1', 'MACOS', 'macos'}):
            raise ValueError('missing macOS deployment version or non-macOS platform')
        headers = run(['/usr/bin/otool', '-arch', arch, '-hv', path], timeout=remaining(deadline)).splitlines()
        filetype = None
        for index, line in enumerate(headers[:-1]):
            if 'filetype' in line.split():
                fields = headers[index + 1].split()
                column = line.split().index('filetype')
                filetype = fields[column] if column < len(fields) else None
        if filetype not in {'EXECUTE', 'DYLIB', 'BUNDLE'}:
            raise ValueError(f'unsupported Mach-O file type: {filetype}')
        slices[arch] = {'minimum_macos': minimum[1], 'install_id': install_id, 'filetype': filetype,
                        'dependencies': dependencies, 'rpaths': rpaths}
    return {'sha256': bounded_hash(path, 1024**3, deadline), 'bytes': path.stat().st_size, 'slices': slices}


def expand(value, loader, executable):
    for prefix, base in [('@loader_path', loader.parent), ('@executable_path', executable.parent)]:
        if value == prefix or value.startswith(prefix + '/'):
            return (base / value[len(prefix):].lstrip('/')).resolve()
    if value.startswith('/'):
        return Path(value).resolve()
    raise ValueError(f'unsupported contextual path: {value}')


def resolve_load(value, loader, executable, rpaths, roots):
    if system_library(value):
        return None
    if value.startswith('@rpath/'):
        candidates = {(base / value[7:]).resolve() for base in rpaths}
        candidates = {path for path in candidates if path.is_file()}
        if len(candidates) != 1:
            raise ValueError(f'@rpath has {len(candidates)} existing candidates; no guessed fallback: {value}')
        path = candidates.pop()
    else:
        path = expand(value, loader, executable)
    if not any(path.is_relative_to(root) for root in roots):
        raise ValueError(f'dependency escapes explicit roots: {value} -> {path}')
    if not path.is_file():
        raise ValueError(f'missing dependency: {value} -> {path}')
    return path


def formula_evidence(path, roots, deadline=None):
    """Record local metadata; formula-wide licenses are not component approval."""
    deadline = deadline if deadline is not None else time.monotonic() + 10
    limit = 2 * 1024**2
    for parent in path.parents:
        if not any(parent.is_relative_to(root) for root in roots):
            break
        receipt = parent / 'INSTALL_RECEIPT.json'
        if not receipt.exists() and not receipt.is_symlink():
            continue
        receipt = regular_file(receipt, limit, roots)
        formula_dir = parent / '.brew'
        if formula_dir.exists() and not any(formula_dir.resolve().is_relative_to(root) for root in roots):
            raise ValueError('formula directory escapes explicit roots')
        formulae = [p for p in directory_entries(formula_dir, 64, deadline) if p.suffix == '.rb'] if formula_dir.exists() else []
        notices = [p for p in directory_entries(parent, 256, deadline)
                   if p.name.lower().startswith(('license', 'copying', 'copyright'))]
        notices = [regular_file(p, limit, roots) for p in notices]
        formula = formulae[0] if len(formulae) == 1 else None
        if formula:
            formula = regular_file(formula, limit, roots)
            with formula.open('rb') as stream:
                data = stream.read(limit + 1)
            if len(data) > limit:
                raise ValueError('formula grew beyond metadata byte limit')
            text = data.decode()
        else:
            text = ''
        license_match = re.search(r'^  license (.*?)(?:\n\n|\n  [a-z])', text, re.M | re.S)
        return {'package_root': str(parent), 'version': parent.name,
                'receipt_sha256': bounded_hash(receipt, limit, deadline),
                'formula': str(formula) if formula else None,
                'formula_sha256': hashlib.sha256(data).hexdigest() if formula else None,
                'formula_license_hint': license_match[1].strip() if license_match else None,
                'dynamic_dependency_hints': [line.strip() for line in text.splitlines()
                                             if 'dlopen' in line or ':no_linkage' in line],
                'top_level_notice_candidates': [{'path': str(p), 'sha256': bounded_hash(p, limit, deadline)} for p in notices],
                'component_license_and_source_provision': 'not reviewed'}
    return None


def audit_runtime(config):
    deadline = time.monotonic() + config.seconds
    version(config.minimum_macos)
    if not 1 <= config.max_images <= 2048 or not 1 <= config.seconds <= 600:
        raise ValueError('audit bounds: 1..2048 images and 1..600 seconds')
    roots = tuple(p.resolve(strict=True) for p in config.roots)
    if not roots or any(not p.is_dir() or p == Path('/') for p in roots):
        raise ValueError('specify dependency directories; filesystem root is not allowed')
    binary, prefix = config.binary.resolve(strict=True), config.prefix.resolve(strict=True)
    if not prefix.is_dir() or not any(prefix.is_relative_to(root) for root in roots):
        raise ValueError('runtime prefix must be inside explicit dependency roots')
    plugins = prefix / 'lib/gstreamer-1.0'
    scanner = prefix / 'libexec/gstreamer-1.0/gst-plugin-scanner'
    report = {'format': 1, 'binary': str(binary), 'runtime_prefix': str(prefix),
              'dependency_roots': list(map(str, roots)), 'architecture': config.architecture,
              'static_closure': {'status': 'incomplete', 'images': {}, 'edges': [], 'issues': []},
              'runtime_profile': {'status': 'unqualified', 'scope': 'all installed plugin candidates; not a required-functionality profile',
                                  'plugin_directory': str(plugins), 'scanner': str(scanner), 'plugin_candidates': [],
                                  'required_evidence': ['source-pinned element/plugin functionality map',
                                                        'dlopen modules including formula hints',
                                                        'plugin/scanner discovery and runtime resources',
                                                        'isolated runtime validation after permitted signing']},
              'deployment_compatibility': {'requested_minimum': config.minimum_macos, 'incompatible': []},
              'redistribution_evidence': {'status': 'unreviewed', 'packages': {}, 'issues': [],
                                        'required_evidence': ['component notices and license texts', 'corresponding source provision',
                                                              'modification/replacement strategy with hardened runtime']},
              'packageable': False, 'mutated_inputs': False, 'executed_inputs': False}
    graph = report['static_closure']
    issues = graph['issues']
    if not plugins.is_dir():
        issues.append({'path': str(plugins), 'error': 'missing plugin directory'})
    seeds = [(binary, binary, ()), (scanner, scanner, ())]
    try:
        plugin_entries = directory_entries(plugins, config.max_images, deadline) if plugins.is_dir() else []
    except ValueError as error:
        plugin_entries = []
        issues.append({'path': str(plugins), 'error': str(error)})
    for plugin in plugin_entries:
        if plugin.suffix not in {'.dylib', '.so'}:
            continue
        report['runtime_profile']['plugin_candidates'].append(str(plugin))
    queue, visited, names = deque(seeds), set(), {}
    arches = ['arm64', 'x86_64'] if config.architecture == 'universal' else [config.architecture]
    queue = deque((p, exe, inherited, arch) for p, exe, inherited in queue for arch in arches)
    while queue:
        if time.monotonic() >= deadline or len(visited) >= config.max_images * 4:
            issues.append({'error': 'audit time/context bound reached'})
            break
        requested, executable, inherited, arch = queue.popleft()
        path = requested.resolve()
        executable = executable.resolve()
        context = (path, executable, inherited, arch)
        if context in visited:
            continue
        visited.add(context)
        try:
            if path != binary and not any(path.is_relative_to(root) for root in roots):
                raise ValueError('image symlink/path escapes explicit roots')
            if str(path) not in graph['images']:
                if len(graph['images']) >= config.max_images:
                    raise ValueError('image count bound reached')
                info = image_info(path, config.architecture, min(10, max(1, deadline - time.monotonic())))
                info['requested_paths'] = [str(requested)]
                graph['images'][str(path)] = info
                other = names.setdefault(path.name.casefold(), path)
                if other != path:
                    issues.append({'path': str(path), 'error': f'flat relocation name collision with {other}'})
                try:
                    evidence = formula_evidence(path, roots, deadline)
                    if evidence:
                        report['redistribution_evidence']['packages'][evidence['package_root']] = evidence
                except (ValueError, OSError) as error:
                    report['redistribution_evidence']['issues'].append({'path': str(path), 'error': str(error)})
                for cpu, data in info['slices'].items():
                    if version(data['minimum_macos']) > version(config.minimum_macos):
                        report['deployment_compatibility']['incompatible'].append(
                            {'path': str(path), 'architecture': cpu, 'minimum_macos': data['minimum_macos']})
            info = graph['images'][str(path)]
            if str(requested) not in info['requested_paths']:
                info['requested_paths'].append(str(requested))
            data = info['slices'][arch]
            bases = tuple(dict.fromkeys([expand(p, path, executable) for p in data['rpaths']] + list(inherited)))
            if any(not any(p.is_relative_to(root) for root in roots) and p != executable.parent for p in bases):
                raise ValueError('rpath escapes dependency roots and executable directory')
            if path == executable and not inherited:
                for candidate in report['runtime_profile']['plugin_candidates']:
                    queue.append((Path(candidate), executable, bases, arch))
            for dep in data['dependencies']:
                edge = {'from': str(path), 'architecture': arch, 'executable_context': str(executable),
                        'rpath_context': list(map(str, bases)), **dep}
                graph['edges'].append(edge)
                try:
                    target = resolve_load(dep['load'], path, executable, bases, roots)
                    edge['resolved'] = str(target) if target else 'system'
                    if target:
                        queue.append((target, executable, bases, arch))
                except (ValueError, OSError) as error:
                    edge['error'] = str(error)
                    issues.append({'path': str(path), 'architecture': arch, 'error': str(error)})
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            issues.append({'path': str(requested), 'error': str(error)})
    graph['status'] = 'incomplete' if issues else 'resolved static loads only'
    graph['total_bytes'] = sum(i['bytes'] for i in graph['images'].values())
    report['deployment_compatibility']['status'] = ('incompatible' if report['deployment_compatibility']['incompatible']
                                                   else 'unknown' if issues else 'compatible inspected images')
    return report
