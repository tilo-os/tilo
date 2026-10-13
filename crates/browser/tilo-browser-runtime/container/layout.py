"""Lay out the web entry from a desktop bundle.

usage: layout.py BUNDLE_DIR SITE_DIR

The bundle's files keep their names, which say nothing about their content,
so the whole bundle goes under one directory named after its content,
`v/<hash>/`, which never changes once published and is cached as immutable.
Only the two entry pages (`index.html`, `app/index.html`) stay at the top,
revalidated on every visit and pointing into that directory; every other
file is found relative to its module (`import.meta.url`), so it follows. The
entry configuration (`entry.json`) is written when the container starts.
"""

import hashlib
import json
import pathlib
import shutil
import sys

bundle, site = map(pathlib.Path, sys.argv[1:])
if site.exists() and any(site.iterdir()):
    sys.exit(f'{site} must be empty')
build = json.loads((bundle / 'build.json').read_text())
files = build['files']
# The version is the bundle's own file hashes (build.json lists every file).
version = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:20]
assets = site / 'v' / version
shutil.copytree(bundle, assets)
pages = {'index.html': './entry-page.mjs', 'app/index.html': '../entry-page.mjs'}
for page, module in pages.items():
    text = (assets / page).read_text()
    statement = f"from '{module}';"
    if text.count(statement) != 1:
        sys.exit(f'{page}: expected one import of {module}')
    prefix = '../' if page.startswith('app/') else './'
    target = site / page
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text.replace(statement, f"from '{prefix}v/{version}/entry-page.mjs';"))
    # Only the top-level page is an entry; a copy under v/ would read the
    # wrong entry.json (its own directory's).
    (assets / page).unlink()
(assets / 'app').rmdir()
(site / 'version.json').write_text(json.dumps({'version': version, 'revision': build['revision']}) + '\n')
print(version)
