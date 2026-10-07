"""Pin the app source's product font catalog beside its native executable."""
import importlib.util
import os
from pathlib import Path
import shutil
import stat
import tomllib

from collection import digest
from collection_local_runtime import floor

DESTINATION = Path('Contents/Resources/fonts')
LOOKUP_LINK = Path('Contents/MacOS/fonts')
LINK_TARGET = '../Resources/fonts'
LIMIT = 64 * 1024**2


def regular(root, name):
    if root.resolve(strict=True) != root:
        raise ValueError('font catalog path must be canonical')
    if not isinstance(name, str):
        raise ValueError('invalid font catalog path')
    relative = Path(name)
    if not name or relative.is_absolute() or str(relative) != name or '..' in relative.parts:
        raise ValueError('invalid font catalog path')
    path = root
    for part in relative.parts:
        path = path / part
        if path.is_symlink():
            raise ValueError('font catalog symlink forbidden')
    if not stat.S_ISREG(path.stat().st_mode) or path.stat().st_size > LIMIT:
        raise ValueError('font catalog requires bounded regular files')
    return path


def make_plan(source):
    root = source / 'crates/sdk/ui-core/assets/fonts'
    if root.resolve(strict=True) != root or not root.is_dir():
        raise ValueError('font catalog root must be canonical')
    manifest = regular(root, 'manifest.toml')
    raw = manifest.read_bytes()
    compiled = regular(source, 'crates/sdk/font-catalog/manifest.toml')
    if raw != compiled.read_bytes():
        raise ValueError('font catalog differs from compiled manifest')
    data = tomllib.loads(raw.decode())
    fonts = [entry for entry in data['font'] if entry['scope'] == 'product']
    entries = fonts + data['license']
    names = [entry['file'] for entry in entries] + ['manifest.toml', 'PROVENANCE.md']
    if not fonts or len(names) > 128 or len(set(names)) != len(names):
        raise ValueError('empty, excessive or duplicate font catalog entries')
    tool = regular(source, 'tools/font-catalog.py')
    spec = importlib.util.spec_from_file_location('qualified_font_catalog', tool)
    catalog = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(catalog)
    if hasattr(catalog, 'prepared_root'):
        root = catalog.prepared_root(root, 'product')
    if (root / 'LICENSES/UPSTREAM.md').is_file():
        names.append('LICENSES/UPSTREAM.md')
    files = {}
    for name in names:
        path = regular(root, name)
        files[str(DESTINATION / name)] = {'source': str(path), 'sha256': digest(path), 'bytes': path.stat().st_size}
    if sum(item['bytes'] for item in files.values()) > LIMIT:
        raise ValueError('font catalog byte bound exceeded')
    count, size = catalog.verify(root, 'product')
    return {'files': files, 'product_count': count, 'product_bytes': size,
            'manifest_sha256': digest(manifest), 'validator_sha256': digest(tool)}


def inspect(app, plan):
    files = plan['files']
    root = app / DESTINATION
    if root.is_symlink() or not root.is_dir():
        raise ValueError('missing regular bundled font catalog directory')
    link = app / LOOKUP_LINK
    if (not link.is_symlink() or os.readlink(link) != LINK_TARGET or
            link.resolve(strict=True) != root):
        raise ValueError('font catalog lookup link changed')
    actual = set()
    for path in root.rglob('*'):
        if path.is_symlink() or (not path.is_dir() and not path.is_file()):
            raise ValueError('unexpected font catalog file type')
        if path.is_file():
            actual.add(str(path.relative_to(app)))
    if actual != set(files):
        raise ValueError('bundled font catalog files differ')
    for relative, item in files.items():
        path = regular(app, relative)
        if os.access(path, os.X_OK) or path.stat().st_size != item['bytes'] or digest(path) != item['sha256']:
            raise ValueError('bundled font catalog bytes or mode changed')


def stage(app, plan):
    root = app / DESTINATION
    for relative in plan['files']:
        path = Path(relative)
        if (str(path) != relative or '..' in path.parts or path.is_absolute() or
                not path.is_relative_to(DESTINATION) or path == DESTINATION):
            raise ValueError('font destination escapes catalog')
    root.mkdir(parents=True, exist_ok=False)
    for relative, item in plan['files'].items():
        floor(app.parent)
        path = Path(item['source'])
        source = regular(path.parent, path.name)
        if source.stat().st_size != item['bytes'] or digest(source) != item['sha256']:
            raise ValueError('pinned source font changed')
        target = app / relative
        if not target.is_relative_to(root):
            raise ValueError('font destination escapes catalog')
        target.parent.mkdir(parents=True, exist_ok=True)
        with source.open('rb') as incoming, target.open('xb') as outgoing:
            shutil.copyfileobj(incoming, outgoing)
        target.chmod(0o644)
    (app / LOOKUP_LINK).symlink_to(LINK_TARGET)
    inspect(app, plan)
