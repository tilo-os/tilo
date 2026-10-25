#!/usr/bin/env python3
"""Page reading and operation through the host's `page` op, in real Chromium."""
import http.server
import json
import re
import sys
import threading
import time
import unittest
from urllib.parse import parse_qs, urlparse
from native_regressions import Host

FORM = b'''<!doctype html><title>Checkout</title>
<style>.visually-hidden{position:absolute;opacity:0;width:1px;height:1px}
#shade{position:fixed;left:0;top:0;width:100%;height:60px;background:#0008}</style>
<header><h1>Fixture shop</h1><nav><a href="/next">Next page</a></nav></header>
<main>
<p>Enter your details.</p>
<form action="/done" method="get">
  <label>Email <input name="email" type="email" required></label>
  <label>Password <input name="password" type="password"></label>
  <label>Country <select name="country"><option value="NL">Netherlands</option><option value="BE">Belgium</option></select></label>
  <input id="terms" name="terms" type="checkbox" class="visually-hidden"><label for="terms">Accept terms</label>
  <label>Search <input name="q" type="search"></label>
  <button>Place order</button>
</form>
<button id="covered" style="position:fixed;top:10px;left:10px">Covered</button>
<div id="shade"></div>
<shop-widget></shop-widget>
<p id="status"></p>
</main>
<script>
customElements.define('shop-widget', class extends HTMLElement {
  connectedCallback() {
    const root = this.attachShadow({mode: 'open'});
    root.innerHTML = '<button>Shadow button</button>';
    root.querySelector('button').onclick = () => document.getElementById('status').textContent = 'shadow clicked';
  }
});
document.querySelector('select').addEventListener('change', e => document.getElementById('status').textContent = 'country ' + e.target.value);
</script>'''


AGENT = b'''<!doctype html><title>Agent</title>
<h1>Agent fixture</h1>
<button id="add" onclick="setTimeout(() => { const p = document.createElement('p'); p.textContent = 'added later'; document.body.append(p); }, 1000)">Add later</button>
<button id="log" onclick="console.log('clicked', 7); fetch('/data.json').then(r => r.text()); setTimeout(() => { throw new Error('boom'); }, 0)">Log</button>
<form id="away" action="https://elsewhere.test/collect" method="get"><label>Query <input name="q"></label><button>Send away</button></form>
<label>Pin <input name="pin" type="password"></label>
'''


# A page whose form is in a child frame, the way a bank's payment widget is.
FRAMED = b'''<!doctype html><title>Framed</title>
<h1>Bank details</h1>
<p>Enter your account in the frame below.</p>
<iframe id="fr" title="Payment frame" src="/widget" style="width:400px;height:300px;border:3px solid #000;padding:2px"></iframe>
<p id="status"></p>'''

WIDGET = b'''<!doctype html><title>Payment</title>
<body style="padding:6px">
<label for="iban">IBAN</label><input type="text" id="iban" name="iban">
<label for="holder">Account holder</label><input type="text" id="holder" name="holder">
<button type="submit" onclick="parent.document.getElementById('status').textContent = 'saved'">Save bank details</button>
</body>'''

# A frame of another site cannot post to the page at all, so it reports what it
# did where only its own world can read it.
FOREIGN = b'''<!doctype html><title>Payment widget</title>
<body style="padding:6px">
<label for="iban">IBAN</label><input type="text" id="iban" name="iban">
<button type="button" onclick="document.title = 'saved ' + document.getElementById('iban').value">Save bank details</button>
</body>'''


PARAGRAPH = ('Reading on a screen is easiest when the line is short, the type is calm and nothing '
             'else competes for attention. ') * 3
ARTICLE = f'''<!doctype html><title>The long read</title>
<style>p {{ color: red; font-size: 9px }}</style>
<nav><a href="/pricing">Pricing</a> <a href="/login">Sign in</a></nav>
<div class="layout"><article><h1>The long read</h1>
<p>{PARAGRAPH}</p><p>{PARAGRAPH}</p><p>{PARAGRAPH}</p>
<aside><a href="/related">Related story</a></aside>
<p>{PARAGRAPH}</p></article>
<div class="sidebar"><a href="/ad">Advertisement</a></div></div>
<input id="kept" value="typed by the person">'''.encode()


# The same page, one of its frames same-origin and one of them another site's.
CROSS = (b'<!doctype html><title>Two frames</title><h1>Two frames</h1>'
         b'<iframe title="Same site" src="/widget" style="width:300px;height:200px"></iframe>'
         b'<iframe title="Other site" src="http://localhost:PORT/foreign" style="width:300px;height:200px"></iframe>')


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = urlparse(self.path).path
        body = FORM if path == '/form' else AGENT if path == '/agent' else ARTICLE if path == '/article' \
            else FRAMED if path == '/framed' else WIDGET if path == '/widget' \
            else FOREIGN if path == '/foreign' \
            else b'{"ok":true}' if path == '/data.json' \
            else f'<title>{path}</title><p>{path}</p>'.encode()
        # A frame of another origin is shown by the same server on another host
        # name, so both documents are reachable and neither can read the other.
        if path == '/cross':
            body = CROSS.replace(b'PORT', str(self.server.server_port).encode())
        self.send_response(200)
        self.send_header('Content-Type', 'application/json' if path == '/data.json' else 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class PageActions(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.h = Host()
        self.addCleanup(self.h.close)
        self.h.send(op='visibility', visible=True)
        self.h.send(op='resize', width=1000, height=800, scale_factor=1.0)
        self.request = 0

    def page(self, action, **fields):
        self.request += 1
        request = self.request
        self.h.send(op='page', request_id=request, action=action, **fields)
        return self.h.until(lambda e: e['event'] == 'page_result' and e['request_id'] == request)

    def ok(self, action, **fields):
        result = self.page(action, **fields)
        self.assertTrue(result['ok'], result)
        return result

    def outline(self):
        return self.ok('outline')['outline']

    def ref(self, outline, pattern):
        match = re.search(pattern + r'.*?\[ref=(d\d+e\d+)\]', outline)
        self.assertIsNotNone(match, f'{pattern!r} not in outline:\n{outline}')
        return match.group(1)

    def main_world(self, expression):
        self.h.send(op='scripts', scripts=['document.title=JSON.stringify({value:(' + expression + '),nonce:' + str(time.monotonic_ns()) + '})'])
        value = json.loads(self.h.until(lambda e: e['event'] == 'title' and e['title'].startswith('{'))['title'])['value']
        self.h.send(op='scripts', scripts=[])
        return value

    def test_outline_lists_structure_with_refs_and_hides_secrets(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        self.assertIn('- banner', outline)
        self.assertIn('heading "Fixture shop" level=1', outline)
        self.assertRegex(outline, r'link "Next page" \[ref=d\d+e\d+\] -> http://127\.0\.0\.1:\d+/next')
        self.assertIn('text "Enter your details."', outline)
        self.assertRegex(outline, r'textbox "Email" \[ref=d\d+e\d+\] type=email required')
        self.assertRegex(outline, r'combobox "Country" \[ref=d\d+e\d+\] value="Netherlands"')
        self.assertIn('option "Belgium"', outline)
        self.assertIn('button "Shadow button"', outline)
        email = self.ref(outline, 'textbox "Email"')
        self.assertEqual(email, self.ref(self.outline(), 'textbox "Email"'), 'refs are stable across reads')
        password = self.ref(outline, 'textbox "Password"')
        refused = self.page('fill', element=password, text='hunter2')
        self.assertEqual((refused['ok'], refused['code']), (False, 'secret_field'), refused)
        self.ok('fill', element=password, text='hunter2', secret=True)
        again = self.outline()
        self.assertIn('value=<7 characters hidden>', again)
        self.assertNotIn('hunter2', again)
        self.assertEqual(self.main_world('typeof globalThis.__tiloPage'), 'undefined',
                         'page script must not reach the automation world')

    def test_reader_view_shows_the_article_alone_and_puts_the_page_back(self):
        self.h.navigate(self.url + '/article')
        self.main_world('document.getElementById("kept").value = "typed later"')
        shown = self.ok('reader', enabled=True)
        self.assertTrue(shown['data']['reader'], shown)
        self.assertGreater(shown['data']['words'], 100, shown)
        outline = self.outline()
        self.assertIn('heading "The long read" level=1', outline)
        self.assertIn('Reading on a screen', outline)
        for gone in ('Pricing', 'Related story', 'Advertisement'):
            self.assertNotIn(gone, outline, f'{gone} is not part of the article')
        self.assertEqual(self.main_world('getComputedStyle(document.body.firstElementChild.shadowRoot'
                                         '.querySelector("p:not(.site)")).fontSize'), '20px',
                         'the page\'s style sheets do not reach the reading layout')
        closed = self.ok('reader', enabled=False)
        self.assertFalse(closed['data']['reader'], closed)
        outline = self.outline()
        self.assertIn('Pricing', outline)
        self.assertEqual(self.main_world('document.getElementById("kept").value'), 'typed later',
                         'the page comes back as it was')
        self.h.navigate(self.url + '/agent')
        refused = self.page('reader', enabled=True)
        self.assertEqual((refused['ok'], refused['code']), (False, 'no_article'), refused)

    def test_fill_select_and_click_operate_the_page_like_a_person(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        self.ok('fill', element=self.ref(outline, 'textbox "Email"'), text='ada@example.test')
        self.ok('select', element=self.ref(outline, 'combobox "Country"'), values=['Belgium'])
        self.ok('click', element=self.ref(outline, 'checkbox "Accept terms"'))
        self.ok('click', element=self.ref(outline, 'button "Shadow button"'))
        after = self.outline()
        self.assertIn('value="ada@example.test"', after)
        self.assertIn('value="Belgium"', after)
        self.assertRegex(after, r'checkbox "Accept terms" \[ref=d\d+e\d+\] checked')
        self.assertIn('text "shadow clicked"', after)
        self.assertEqual(self.main_world("new FormData(document.forms[0]).get('country')"), 'BE')
        missing = self.page('select', element=self.ref(outline, 'combobox "Country"'), values=['France'])
        self.assertEqual((missing['ok'], missing['code']), (False, 'no_option'), missing)
        covered = self.page('click', element=self.ref(outline, 'button "Covered"'))
        self.assertEqual((covered['ok'], covered['code']), (False, 'obscured'), covered)
        self.assertIn('shade', covered['message'])
        not_text = self.page('fill', element=self.ref(outline, 'button "Place order"'), text='x')
        self.assertEqual(not_text['code'], 'not_editable', not_text)

    def test_submit_validates_and_enter_submits_through_a_navigation(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        search = self.ref(outline, 'searchbox "Search"')
        invalid = self.page('submit', element=search)
        self.assertEqual((invalid['ok'], invalid['code']), (False, 'invalid_form'), invalid)
        self.assertIn(self.ref(outline, 'textbox "Email"'), invalid['message'])
        self.ok('fill', element=self.ref(outline, 'textbox "Email"'), text='ada@example.test')
        self.ok('fill', element=search, text='tea')
        pressed = self.ok('press', element=search, key='Enter', ctrl=False, shift=False, alt=False, meta=False)
        # The reply waits for the navigation Enter started, so it names the new page.
        self.assertEqual((pressed['navigated'], pressed['loading']), (True, False), pressed)
        self.assertTrue(any(e['event'] == 'loaded' and '/done' in e['url'] for e in self.h.seen))
        query = parse_qs(urlparse(pressed['url']).query)
        self.assertEqual((query['email'], query['q']), (['ada@example.test'], ['tea']))
        stale = self.page('click', element=search)
        self.assertEqual((stale['ok'], stale['code']), (False, 'stale_element'), stale)

    def test_clicking_a_link_reports_the_navigation_it_started(self):
        self.h.navigate(self.url + '/form')
        link = self.ref(self.outline(), 'link "Next page"')
        clicked = self.ok('click', element=link)
        self.assertEqual((clicked['navigated'], clicked['loading']), (True, False), clicked)
        self.assertTrue(clicked['url'].endswith('/next'), clicked)
        self.assertIn('text "/next"', self.outline())
        idle = self.ok('press', key='Tab', ctrl=False, shift=False, alt=False, meta=False)
        self.assertFalse(idle['navigated'], idle)


    def test_changes_report_only_what_appeared_after_the_page_settles(self):
        self.h.navigate(self.url + '/agent')
        add = self.ref(self.outline(), 'button "Add later"')
        self.ok('click', element=add)
        # The page adds its line a second after the click; settling waits for it.
        settled = self.ok('settle', quiet_ms=1500, timeout_ms=8000)
        self.assertTrue(settled['data']['settled'], settled)
        self.assertGreater(settled['data']['mutations'], 0, settled)
        changed = self.ok('changes')
        self.assertEqual(changed['outline'], '+ - text "added later"', changed)
        self.assertEqual(self.ok('changes')['message'], 'Nothing changed')

    def test_element_at_names_what_a_click_there_would_hit(self):
        self.h.navigate(self.url + '/agent')
        add = self.ref(self.outline(), 'button "Add later"')
        x, y = self.main_world("(r => [r.x + r.width / 2, r.y + r.height / 2])(document.getElementById('add').getBoundingClientRect())")
        hit = self.ok('at', x=x, y=y)
        self.assertEqual((hit['data']['ref'], hit['data']['interactive']), (add, True), hit)
        empty = self.ok('at', x=990, y=790)
        self.assertFalse(empty['data']['interactive'], empty)

    def test_console_and_network_reach_the_agent_and_not_the_page(self):
        self.h.navigate(self.url + '/agent')
        log = self.ref(self.outline(), 'button "Log"')
        self.ok('click', element=log)
        self.ok('settle', quiet_ms=300, timeout_ms=5000)
        console = self.ok('console', since=0)['data']
        texts = [entry['text'] for entry in console['entries']]
        self.assertIn('clicked 7', texts, console)
        self.assertTrue(any('boom' in entry['text'] and entry.get('uncaught') for entry in console['entries']), console)
        self.assertEqual(console['provenance'], 'web-content')
        network = self.ok('network', since=0)['data']
        data = [entry for entry in network['entries'] if entry['url'].endswith('/data.json')]
        self.assertEqual(len(data), 1, network)
        self.assertEqual((data[0]['status'], data[0]['state']), (200, 'done'), data)
        self.assertNotIn('headers', data[0])
        self.assertEqual(self.ok('console', since=console['next'])['data']['entries'], [], 'nothing new since')
        self.assertEqual(self.main_world('typeof globalThis.__tiloPage'), 'undefined')

    def test_secrets_and_cross_site_submissions_need_explicit_consent(self):
        self.h.navigate(self.url + '/agent')
        outline = self.outline()
        pin = self.ref(outline, 'textbox "Pin"')
        self.assertEqual(self.page('fill', element=pin, text='1234')['code'], 'secret_field')
        self.ok('fill', element=pin, text='1234', secret=True)
        query = self.ref(outline, 'textbox "Query"')
        for action, fields in (('submit', {}), ('click', {})):
            target = query if action == 'submit' else self.ref(outline, 'button "Send away"')
            refused = self.page(action, element=target, **fields)
            self.assertFalse(refused['ok'], (action, refused))
            self.assertEqual((refused['ok'], refused['code']), (False, 'cross_origin_submit'), refused)
            self.assertIn('elsewhere.test', refused['message'])
        entered = self.page('press', element=query, key='Enter', ctrl=False, shift=False, alt=False, meta=False)
        self.assertEqual(entered['code'], 'cross_origin_submit', entered)


class FramedActions(unittest.TestCase):
    """A page whose controls live in child frames, reached through the actions."""

    def ref_of(self, outline, pattern):
        """A ref describing an element inside a child frame."""
        match = re.search(pattern + r'.*?\[ref=(f\d+d\d+e\d+)\]', outline)
        self.assertIsNotNone(match, f'{pattern!r} not in outline:\n{outline}')
        return match.group(1)

    # The same fixture and helpers as the page tests, against the same server.
    # A classmethod copied as an attribute stays bound to its own class, so it
    # is wrapped to bind to this one.
    setUpClass = classmethod(lambda cls: PageActions.setUpClass.__func__(cls))
    tearDownClass = classmethod(lambda cls: PageActions.tearDownClass.__func__(cls))
    setUp = PageActions.setUp
    tearDown = PageActions.tearDown
    page, ok, outline, main_world = PageActions.page, PageActions.ok, PageActions.outline, PageActions.main_world

    def test_outline_lists_a_frame_and_its_own_lines_under_it(self):
        self.h.navigate(self.url + '/framed')
        outline = self.outline()
        self.assertRegex(outline, r'- iframe "Payment frame" \[ref=(f1d\d+e\d+)\]')
        # The frame's own lines follow the line naming it, indented below it.
        lines = outline.splitlines()
        at = next(i for i, line in enumerate(lines) if 'iframe "Payment frame"' in line)
        below = lines[at + 1:at + 5]
        self.assertTrue(any('textbox "IBAN"' in line for line in below), below)
        self.assertTrue(any(line.startswith('      - ') for line in below), below)
        self.assertRegex(outline, r'button "Save bank details" \[ref=f1d\d+e\d+\]')
        # The frame's refs name the frame they were read in.
        self.assertRegex(outline, r'textbox "IBAN" \[ref=f1d\d+e\d+\]')
        # The page's own controls keep the refs they always had.
        self.assertRegex(outline, r'heading "Bank details" level=1')

    def test_actions_on_a_frame_ref_run_in_that_frame(self):
        self.h.navigate(self.url + '/framed')
        outline = self.outline()
        self.ok('fill', element=self.ref_of(outline, 'textbox "IBAN"'), text='EX89370400440532013000')
        self.ok('fill', element=self.ref_of(outline, 'textbox "Account holder"'), text='Maria Elena Fischer')
        after = self.outline()
        self.assertIn('value="EX89370400440532013000"', after)
        self.assertIn('value="Maria Elena Fischer"', after)
        # A trusted click reaches the frame's button, and what it did reaches
        # the page: the page's own script ran from it.
        self.ok('click', element=self.ref_of(after, 'button "Save bank details"'))
        self.ok('settle', quiet_ms=300, timeout_ms=5000)
        self.assertIn('text "saved"', self.outline())
        # Reading a frame is still reading: page script cannot call the world.
        self.assertEqual(self.main_world('typeof globalThis.__tiloPage'), 'undefined')

    def test_each_frame_is_read_in_its_own_world(self):
        self.h.navigate(self.url + '/cross')
        outline = self.outline()
        self.assertRegex(outline, r'- iframe "Same site" \[ref=f1d\d+e\d+\]')
        self.assertRegex(outline, r'- iframe "Other site" \[ref=f2d\d+e\d+\]')
        # A frame of another site is read exactly as one of this site's is.
        self.assertRegex(outline, r'textbox "IBAN" \[ref=f2d\d+e\d+\]')
        self.assertNotIn('f1d', outline.split('iframe "Other site"')[1], 'the two frames keep their own refs')
        iban = self.ref_of(outline, 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89')
        # The foreign frame's own world saw the text its own document holds.
        self.assertIn('value="EX89"', self.outline().split('iframe "Other site"')[1])
        self.assertNotIn('value="EX89"', self.outline().split('iframe "Other site"')[0])


if __name__ == '__main__':
    # `native_regressions` reads the host path at its own import, so only the
    # test selector is left here.
    wanted = sys.argv[1:] or ['PageActions', 'FramedActions']
    loader = unittest.TestLoader()
    suite = unittest.TestSuite(loader.loadTestsFromNames(wanted, sys.modules[__name__]))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)
