"""The web entry's layout: the pages, the versioned directory, and the module hints.

layout.py runs in the web entry's build context (tools/ci/publish/web-entry-context.sh): it
moves a desktop bundle under `v/<hash>/` and writes the two entry pages. What a first visit
downloads depends on what it writes, so these run the production script over a bundle made of
the real entry pages and a module graph like the bundle's (three and four levels deep), and
then over the runtime's own modules, to check the graph the shipped entry module gets.
"""
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
CONTAINER = ROOT / 'crates/browser/tilo-browser-runtime/container'
WEB = ROOT / 'crates/browser/tilo-browser-runtime/web'
LAYOUT = CONTAINER / 'layout.py'

# The parts of layout.py a test reads the module graph with, as it runs them there: the
# script lays its bundle out as it is imported, so its graph code is taken by the name the
# module gives it.
SOURCE = LAYOUT.read_text()


def graph_functions():
    """layout.py's regexes and graph functions, without laying anything out."""
    start = SOURCE.index('STRING = ')
    end = SOURCE.index('\nbundle, site = ')
    namespace = {'re': re, 'os': __import__('os'), 'pathlib': pathlib}
    exec(compile(textwrap.dedent(SOURCE[start:end]), str(LAYOUT), 'exec'), namespace)
    return namespace


class Layout(unittest.TestCase):
    def bundle(self, root):
        """A bundle: the real entry pages, and a module graph as deep as the shipped one."""
        bundle = root / 'bundle'
        (bundle / 'pkg').mkdir(parents=True)
        for page in ('index.html', 'app/index.html'):
            (bundle / page).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(WEB / page, bundle / page)
        # entry-page.mjs imports two modules, each of them two more, and one of
        # those a fourth; media-moq-library.mjs is the one the page names itself,
        # and the rest is what a module may mention without importing.
        modules = {
            'entry-page.mjs': "import {a} from './host.mjs';\n"
                              "export * from './arrival.mjs';\n"
                              "const lazy = () => import('./never-preloaded.mjs');\n",
            'arrival.mjs': "import './desktop.mjs';\n",
            'host.mjs': "import {b} from './session/peer.mjs';\nimport './session/live.mjs';\n"
                        "import {\n  c,\n  d,\n} from './table.mjs';\n",
            'table.mjs': "export const table = 1;\n",
            'session/peer.mjs': "export {c} from './live.mjs';\nimport './buffers.mjs';\n",
            # A comment and a string import nothing; the inline module may.
            'session/live.mjs': "// import './in-a-comment.mjs'\n"
                                "const s = \"import './in-a-string.mjs'\";\n"
                                "import 'data:text/javascript,export default 1';\n",
            'session/buffers.mjs': "export const buffers = 1;\n",
            'desktop.mjs': "export const desktop = 1;\n",
            'never-preloaded.mjs': "export const never = 1;\n",
            'media-moq-library.mjs': "export const moq = 1;\n",
            'in-a-comment.mjs': "export const no = 1;\n",
            'in-a-string.mjs': "export const no = 1;\n",
        }
        for name, text in modules.items():
            module = bundle / name
            module.parent.mkdir(parents=True, exist_ok=True)
            module.write_text(text)
        (bundle / 'pkg/tilo_browser_runtime_bg.wasm').write_bytes(b'\0asm')
        (bundle / 'apple-touch-icon.png').write_bytes(b'\x89PNG')
        (bundle / 'TiloSans.ttf').write_bytes(b'\0\1\0\0')
        files = {}
        for path in sorted(bundle.rglob('*')):
            if path.is_file():
                files[path.relative_to(bundle).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
        (bundle / 'build.json').write_text(json.dumps({'files': files, 'revision': 'a' * 40}))
        return bundle

    def lay_out(self, root):
        bundle = self.bundle(root)
        site = root / 'site'
        result = subprocess.run([sys.executable, '-I', str(LAYOUT), str(bundle), str(site)],
                                capture_output=True, text=True)
        return result, site

    def page(self, site, name):
        return (site / name).read_text()

    def hints(self, site, page):
        return re.findall(r'<link rel="modulepreload" href="([^"]+)">', self.page(site, page))

    # The entry module's whole static graph, in the order a browser meets it: the
    # page's own modulepreload of the library, then the entry module and the
    # modules it imports, each level in import order. Dynamic imports, comments
    # and strings name no module.
    EXPECTED = ('media-moq-library.mjs', 'entry-page.mjs', 'host.mjs', 'arrival.mjs',
                'session/peer.mjs', 'session/live.mjs', 'table.mjs', 'desktop.mjs',
                'session/buffers.mjs')

    def test_every_module_the_entry_module_reaches_is_a_hint(self):
        with tempfile.TemporaryDirectory() as directory:
            result, site = self.lay_out(pathlib.Path(directory))
            self.assertEqual(result.returncode, 0, result.stderr)
            version = result.stdout.strip()
            self.assertEqual(self.hints(site, 'index.html'),
                             [f'./v/{version}/{name}' for name in self.EXPECTED])

    def test_the_app_entry_page_points_one_level_up(self):
        with tempfile.TemporaryDirectory() as directory:
            result, site = self.lay_out(pathlib.Path(directory))
            self.assertEqual(result.returncode, 0, result.stderr)
            version = result.stdout.strip()
            hints = self.hints(site, 'app/index.html')
            self.assertEqual(hints, [f'../v/{version}/{name}' for name in self.EXPECTED])
            for href in re.findall(r'href="([^"]+)"', self.page(site, 'app/index.html')):
                if href.startswith('data:'):
                    continue
                self.assertTrue((site / 'app' / href).is_file(), href)

    def test_the_wasm_preload_follows_into_the_versioned_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            result, site = self.lay_out(pathlib.Path(directory))
            self.assertEqual(result.returncode, 0, result.stderr)
            version = result.stdout.strip()
            for page, prefix in (('index.html', './'), ('app/index.html', '../')):
                preloads = re.findall(r'<link rel="preload" href="([^"]+)"', self.page(site, page))
                self.assertEqual(preloads, [f'{prefix}v/{version}/pkg/tilo_browser_runtime_bg.wasm'])

    def test_a_dynamic_import_is_not_preloaded(self):
        with tempfile.TemporaryDirectory() as directory:
            result, site = self.lay_out(pathlib.Path(directory))
            self.assertEqual(result.returncode, 0, result.stderr)
            hints = ''.join(self.hints(site, 'index.html'))
            self.assertNotIn('never-preloaded', hints)
            self.assertNotIn('in-a-comment', hints)
            self.assertNotIn('in-a-string', hints)

    def test_the_hints_follow_the_ones_the_page_was_written_with(self):
        with tempfile.TemporaryDirectory() as directory:
            result, site = self.lay_out(pathlib.Path(directory))
            self.assertEqual(result.returncode, 0, result.stderr)
            text = self.page(site, 'index.html')
            wasm = text.index('rel="preload"')
            for hint in self.hints(site, 'index.html'):
                self.assertLess(wasm, text.index(hint))
            self.assertEqual(len(set(self.hints(site, 'index.html'))), len(self.EXPECTED))

    def test_the_layout_is_deterministic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            first, site = self.lay_out(root / 'one')
            self.assertEqual(first.returncode, 0, first.stderr)
            one = {path.relative_to(site): path.read_bytes()
                   for path in sorted(site.rglob('*')) if path.is_file()}
            shutil.rmtree(root / 'one')
            second, site = self.lay_out(root / 'two')
            self.assertEqual(second.returncode, 0, second.stderr)
            other = {path.relative_to(site): path.read_bytes()
                     for path in sorted(site.rglob('*')) if path.is_file()}
            self.assertEqual(one, other, 'the same bundle lays out the same version, byte for byte')

    def test_a_module_the_entry_module_imports_but_the_bundle_lacks_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            bundle = self.bundle(root)
            (bundle / 'desktop.mjs').unlink()
            build = json.loads((bundle / 'build.json').read_text())
            del build['files']['desktop.mjs']
            (bundle / 'build.json').write_text(json.dumps(build))
            result = subprocess.run([sys.executable, '-I', str(LAYOUT), str(bundle),
                                     str(root / 'site')], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('desktop.mjs', result.stderr)

    def test_a_page_without_its_imports_is_refused(self):
        # A page that stops importing the entry module (or imports it twice)
        # would lay out to a page that loads nothing; layout.py says so.
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name, statement in (('index.html', ""),
                                    ('app/index.html', "from '../entry-page.mjs';\n"
                                                       "from '../entry-page.mjs';\n")):
                bundle = self.bundle(root / name.replace('/', '-'))
                page = bundle / name
                text = page.read_text()
                marker = "from './entry-page.mjs';" if name == 'index.html' else "from '../entry-page.mjs';"
                text = text.replace(marker, statement)
                page.write_text(text)
                result = subprocess.run([sys.executable, '-I', str(LAYOUT), str(bundle),
                                         str(root / f'site-{name.replace("/", "-")}')],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, name)
                self.assertIn('import', result.stderr, name)


class ShippedGraph(unittest.TestCase):
    """The graph layout.py reads out of the runtime's own modules."""

    @unittest.skipUnless((WEB / 'entry-page.mjs').is_file(), 'the runtime sources are not here')
    def test_the_shipped_entry_module_reaches_the_editor_through_static_imports(self):
        layout = graph_functions()
        graph = layout['graph'](WEB / 'entry-page.mjs')
        self.assertGreater(len(graph), 40, 'the entry module reaches the editor')
        for module in graph:
            self.assertTrue(module.is_file(), module)
        names = {module.name for module in graph}
        self.assertIn('host.mjs', names)
        self.assertIn('entry-page.mjs', names)
        # A module a dynamic import names is preloaded only if a static import
        # reaches it too: the library the transports load on demand, and the
        # relay a page connects to when it has one, are not preloaded.
        self.assertNotIn('media-moq-library.mjs', names)
        self.assertNotIn('relay.mjs', names)

    @unittest.skipUnless((WEB / 'entry-page.mjs').is_file(), 'the runtime sources are not here')
    def test_every_hint_is_a_file_of_the_runtime(self):
        layout = graph_functions()
        for module in layout['graph'](WEB / 'entry-page.mjs'):
            self.assertTrue(module.resolve().is_relative_to(WEB.resolve()), module)
            self.assertEqual(module.suffix, '.mjs', module)


if __name__ == '__main__':
    unittest.main()
