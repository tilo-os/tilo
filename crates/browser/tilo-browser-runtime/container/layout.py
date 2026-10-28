"""Lay out the web entry from a desktop bundle.

usage: layout.py BUNDLE_DIR SITE_DIR

The bundle's files keep their names, which say nothing about their content,
so the whole bundle goes under one directory named after its content,
`v/<hash>/`, which never changes once published and is cached as immutable.
Only the two entry pages (`index.html`, `app/index.html`) stay at the top,
revalidated on every visit and pointing into that directory, and beside them
their home-screen icon (`apple-touch-icon.png`), where Safari also looks for
it unasked; every other file is found relative to its module
(`import.meta.url`), so it follows. The entry configuration (`entry.json`) is
written when the container starts.

A first visit starts at one module, `entry-page.mjs`, and the browser learns
of the modules it imports only after downloading and parsing it: at sixty-odd
modules three and four levels deep that is a waterfall of round trips before
the editor runs. So the page names every module `entry-page.mjs` statically
imports, in the order a browser meets them, as `<link rel="modulepreload">`
next to the hints the page was written with; each is then fetched at once,
alongside the start module.
"""

import hashlib
import json
import os
import pathlib
import re
import shutil
import sys

# Text in a string is not code, and neither is a comment: either can read like
# an import. What a string keeps is a module's name, where one names a module
# (`code_of`). A quote stands for itself alone as a one-character class, which
# needs no escape in a raw pattern, and an escaped quote would.
STRING = re.compile(
    r'''(?<![\\\w])['][^'\n]*[']'''
    + r'''|(?<![\\\w])`[^`]*`|//[^\n]*|/\*.*?\*/''',
    re.S,
)
# A static import of a module for its side effects (`import './a.mjs';`). A
# dynamic import runs from code, after a `=` or a `(`, and is not one.
SIDE_EFFECT = re.compile(r'''(?<![=(,])\bimport\s+['"]([^'"]+)['"]''')
# An import or re-export clause, up to the `from` its specifier follows: a
# default or named binding (`import a from`, `import a, {b} from`), a bare
# `import from`, or a re-export (`export * from`, `export {a} from`). Names and
# braces have no `from` of their own, and a list of names may break across
# lines.
CLAUSE = re.compile(
    r'''(?:\bimport\s*(?:\{[^}]*\}|[\w$*]+)?|\bexport\s*(?:\*|\{[^}]*\}))\s*from\b''',
    re.S,
)
# The specifier a clause is followed by.
SPECIFIER = re.compile(r'''\s*['"]([^'"]+)['"]''')


def code_of(path):
    """`path`'s source, with its comments and its strings' text blanked.

    One string keeps its text: the one an import names a module with.
    """
    def text_of(match):
        kept = after.match(match.string, match.end())
        quote = match.group(0)
        return quote if kept else quote[0] + ' ' * (len(quote) - 2) + quote[-1]

    # What a specifier's string is followed by, and no other string is.
    after = re.compile(r'''\s*[;),]''')
    return STRING.sub(text_of, path.read_text())


def static_imports(module):
    """The modules `module` statically imports, in the order they are written.

    Dynamic imports are not: each runs when some path reaches it, and what a
    session may never reach is bytes on every first load if it is preloaded.
    """
    code = code_of(module)
    entries = [(m.start(), m.group(1)) for m in SIDE_EFFECT.finditer(code)]
    for clause in CLAUSE.finditer(code):
        specifier = SPECIFIER.match(code, clause.end())
        if specifier:
            entries.append((clause.start(), specifier.group(1)))
    found = []
    for _, specifier in sorted(entries):
        if not specifier.startswith(('.', '/')):
            continue
        target = pathlib.Path(os.path.normpath(str(module.parent / specifier)))
        if all(str(target) != str(other) for other in found):
            found.append(target)
    return found


def graph(entry):
    """Every module `entry` statically reaches, breadth first.

    That is the order a browser meets them too: the entry module's own imports
    before theirs, each level in the order they are written, so a page that
    names them all starts their fetches at once instead of a level per round
    trip.
    """
    reached = []
    frontier = [entry]
    while frontier:
        reached.extend(frontier)
        following = []
        for module in frontier:
            for target in static_imports(module):
                if target not in reached and target not in following:
                    following.append(target)
        frontier = following
    return reached


bundle, site = map(pathlib.Path, sys.argv[1:])
if site.exists() and any(site.iterdir()):
    sys.exit(f'{site} must be empty')
build = json.loads((bundle / 'build.json').read_text())
files = build['files']
# The version is the bundle's own file hashes (build.json lists every file).
version = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:20]
assets = site / 'v' / version
shutil.copytree(bundle, assets)

modules = graph(assets / 'entry-page.mjs')
# The bundle is the bundle: a module the entry module imports and it lacks is
# a broken build, not a page to serve.
missing = [module for module in modules if not module.is_file()]
missing += [target for module in modules for target in static_imports(module)
            if not target.is_file()]
if missing:
    sys.exit(f'{missing[0].relative_to(assets)} is imported but not in the bundle')
names = [module.relative_to(assets).as_posix() for module in modules]

# Each page imports the entry module by a path of its own, and names a few
# files by path besides; all of them are in the versioned directory.
pages = {'index.html': './entry-page.mjs', 'app/index.html': '../entry-page.mjs'}
for page, entry in pages.items():
    prefix = '../' if page.startswith('app/') else './'
    text = (assets / page).read_text()
    statement = f"from '{entry}';"
    if text.count(statement) != 1:
        sys.exit(f'{page}: expected one import of {entry}')
    text = text.replace(statement, f"from '{prefix}v/{version}/entry-page.mjs';")
    for rel in ('preload', 'modulepreload'):
        hint = f'<link rel="{rel}" href="{prefix}'
        text = text.replace(hint, f'{hint}v/{version}/')
    # The library the page preloads itself comes first, as it was written:
    # every static import of the entry module follows it in the graph's order.
    media = 'media-moq-library.mjs'
    anchor = f'<link rel="modulepreload" href="{prefix}v/{version}/{media}">'
    if text.count(anchor) != 1:
        sys.exit(f'{page}: expected one modulepreload of {media}')
    # A module the page already names itself (the entry module, preloaded so
    # a page laid out without this step still starts it at once) is named
    # once, in the graph's place for it.
    for name in names:
        text = re.sub(r'\n[ \t]*' + re.escape(f'<link rel="modulepreload" href="{prefix}v/{version}/{name}">'),
                      '', text)
    hints = ''.join(f'\n  <link rel="modulepreload" href="{prefix}v/{version}/{name}">'
                    for name in [media, *names])
    text = text.replace(anchor, hints)
    (site / page).parent.mkdir(parents=True, exist_ok=True)
    (site / page).write_text(text)
    # Only the pages at the top are entries: a copy under v/ would be served
    # with the entry.json of its own directory, which is not the home one.
    (assets / page).unlink()
(assets / 'app').rmdir()
(assets / 'apple-touch-icon.png').rename(site / 'apple-touch-icon.png')
(site / 'version.json').write_text(json.dumps({'version': version, 'revision': build['revision']}) + '\n')
print(version)
