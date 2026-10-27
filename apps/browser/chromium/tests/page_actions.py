#!/usr/bin/env python3
"""Page reading and operation through the host's `page` op, in real Chromium."""
import base64
import http.server
import json
import os
import re
import tempfile
import threading
import time
import unittest
from urllib.parse import parse_qs, urlparse
from native_regressions import HANG_BOUND, Host

# The longest settle the host accepts (host.cc's settle validation); a
# longer one is a malformed command and the host closes.
SETTLE_MS = min(int(HANG_BOUND * 1000), 20000)

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
  <label>Proof of address <input name="proof" type="file" accept=".pdf"></label>
  <label>Receipts <input name="receipts" type="file" multiple></label>
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
for (const field of document.querySelectorAll('input[type=file]'))
  field.addEventListener('change', e => document.getElementById('status').textContent =
    'files ' + [...e.target.files].map(f => f.name + ':' + f.size).join(','));
</script>'''


AGENT = b'''<!doctype html><title>Agent</title>
<h1>Agent fixture</h1>
<button id="add" onclick="setTimeout(() => { const p = document.createElement('p'); p.textContent = 'added later'; document.body.append(p); }, 1000)">Add later</button>
<button id="log" onclick="console.log('clicked', 7); fetch('/data.json').then(r => r.text()); setTimeout(() => { throw new Error('boom'); }, 0)">Log</button>
<form id="away" action="https://elsewhere.test/collect" method="get"><label>Query <input name="q"></label><button>Send away</button></form>
<label>Pin <input name="pin" type="password"></label>
'''


CANVAS = b'''<!doctype html><title>Sign</title>
<canvas id="c" width="500" height="150" aria-label="Signature pad" style="border:1px solid #000"></canvas>
<button type="button" id="s">Submit signature</button><p id="m"></p>
<script>let ink=0,down=false;const x=document.getElementById("c").getContext("2d");
const c=document.getElementById("c");
c.onpointerdown=e=>{down=true;x.beginPath();x.moveTo(e.offsetX,e.offsetY)};
c.onpointermove=e=>{if(down){x.lineTo(e.offsetX,e.offsetY);x.stroke();ink++}};
window.onpointerup=()=>down=false;
window.strokes=()=>ink;
document.getElementById("s").onclick=()=>{document.getElementById("m").textContent=ink<5?"Draw your signature first":"Signature received"};
</script>'''


GESTURES = b'''<!doctype html><title>Gestures</title>
<style>.menu{position:relative;display:inline-block}
.menu ul{display:none;position:absolute;background:#fff;list-style:none;padding:8px;margin:0}
.menu:hover ul{display:block}
li{border:1px solid #000;padding:8px;margin:4px;list-style:none}</style>
<h1>Gestures</h1>
<nav class="menu"><span id="trigger" style="padding:8px;display:inline-block;border:1px solid #000">Licences menu</span>
<ul><li><a href="#" onclick="opened(\'fishing\');return false">Fishing licence</a></li></ul></nav>
<ol id="l"><li draggable="true">Riverton office</li><li draggable="true">Porthaven office</li><li draggable="true">Central office</li></ol>
<canvas id="c" width="400" height="150" style="border:2px solid #000" aria-label="Signature pad"></canvas>
<button type="button" id="s" onclick="submitSignature()">Submit signature</button>
<button type="button" id="save" onclick="saveOrder()">Save order</button>
<p id="m"></p>
<script>
let ink = 0, down = false;
const pad = document.getElementById('c'), ctx = pad.getContext('2d');
pad.onpointerdown = e => { down = true; ctx.beginPath(); ctx.moveTo(e.offsetX, e.offsetY); };
pad.onpointermove = e => { if (!down) return; ctx.lineTo(e.offsetX, e.offsetY); ctx.stroke(); ink++; };
pad.onpointerup = () => { down = false; };
window.drawn = () => ink;
window.opened = name => { document.getElementById('m').textContent = 'opened ' + name; };
window.order = () => [...document.getElementById('l').children].map(x => x.textContent).join(' > ');
window.saveOrder = () => { document.getElementById('m').textContent = 'saved ' + order(); };
window.submitSignature = () => { document.getElementById('m').textContent = ink < 5 ? 'Draw your signature first' : 'Signature received'; };
let held;
document.querySelectorAll('#l li').forEach(li => {
  li.ondragstart = () => { held = li; };
  li.ondragover = e => e.preventDefault();
  li.ondrop = e => { e.preventDefault(); if (held !== li) document.getElementById('l').insertBefore(held, li.nextSibling); };
});
</script>'''


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


LATER = b'''<!doctype html><title>Later</title>
<button onclick="setTimeout(() => location.href = '/next', 1000)">Go later</button>'''
GESTURE = b'''<!doctype html><title>Gestures</title>
<style>
#menu{display:none} #hover:hover #menu{display:block}
li{padding:8px;margin:4px;border:1px solid #000;list-style:none}
</style>
<div id="hover"><span id="label">Licences</span><ul id="menu"><li><a href="/fishing" id="fishing">Fishing licence</a></li></ul></div>
<ol id="rank"><li draggable="true" id="a">Riverton office</li><li draggable="true" id="b">Porthaven office</li><li draggable="true" id="c">Central office</li></ol>
<canvas id="pad" width="300" height="120" aria-label="Signature pad" style="border:1px solid #000"></canvas>
<p id="status"></p>
<script>
const rank = document.getElementById('rank');
let dragged;
rank.querySelectorAll('li').forEach(li => {
  li.ondragstart = () => dragged = li;
  li.ondragover = e => e.preventDefault();
  li.ondrop = e => { e.preventDefault(); if (dragged !== li) rank.insertBefore(dragged, li); };
});
const pad = document.getElementById('pad'), ctx = pad.getContext('2d');
let drawing = false, strokes = 0;
pad.onpointerdown = e => { drawing = true; strokes++; ctx.beginPath(); ctx.moveTo(e.offsetX, e.offsetY); };
pad.onpointermove = e => { if (drawing) ctx.lineTo(e.offsetX, e.offsetY); ctx.stroke(); };
pad.onpointerup = () => { drawing = false; document.getElementById('status').textContent = 'strokes ' + strokes; };
</script>'''


FRAMED = b'''<!doctype html><title>Framed</title>
<h1>Framed page</h1>
<iframe src="/widget" title="Payment form" style="width:600px;height:240px;border:0"></iframe>
<button id="outside">Outside the frame</button>
'''


# A frame holding a frame: the outer one is frame 1, the inner one frame 1.1.
NESTED = b'''<!doctype html><title>Nested</title>
<h1>Nested frames</h1>
<iframe src="/outer" title="Outer frame" style="width:600px;height:400px;border:0"></iframe>
'''


OUTER = b'''<!doctype html><title>Outer</title>
<p>Outer document</p>
<iframe src="/widget" title="Payment form" style="width:560px;height:240px;border:0"></iframe>
'''


LATE = b'''<!doctype html><title>Late frame</title>
<h1>Late page</h1>
<p>The frame is made by script, after the page has loaded.</p>
<script>setTimeout(() => {
  const fr = document.createElement('iframe');
  fr.title = 'Late payment form';
  fr.src = '/widget-late';
  fr.style.cssText = 'width:600px;height:240px;border:0';
  document.body.append(fr);
}, 300);</script>
'''


# The frame's first document moves on to the form by itself, so the frame the
# page attached is navigated again before an action can name it.
WIDGET_LATE = b'''<!doctype html><title>Connecting</title>
<p id="state">Connecting to the payment service</p>
<script>setTimeout(() => location.replace('/widget'), 200);</script>
'''


WIDGET = b'''<!doctype html><title>Payment details</title>
<form action="/done" method="get">
  <label>IBAN <input name="iban"></label>
  <label>Account holder <input name="holder"></label>
  <button>Save bank details</button>
</form>
<canvas id="pad" width="200" height="120" style="width:100px;height:60px"></canvas>
<p id="pressed"></p>
<script>
pad.onpointerdown = e => {
  const left = pad.getBoundingClientRect().left, top = pad.getBoundingClientRect().top;
  document.getElementById('pressed').textContent =
    'pressed ' + Math.round(left + e.offsetX) + ',' + Math.round(top + e.offsetY);
};
</script>
'''


RICH = rb'''<!doctype html><title>Rich text</title>
<label id="note-label">Note</label>
<div id="note" contenteditable="true" role="textbox" aria-multiline="true" aria-labelledby="note-label"></div>
<label id="draft-label">Draft</label>
<div id="draft" contenteditable="true" role="textbox" aria-multiline="true" aria-labelledby="draft-label"></div>
<label id="report-label">Report</label>
<div id="report" contenteditable="true" role="textbox" aria-multiline="true" aria-labelledby="report-label"></div>
<label>Steps <textarea id="steps" aria-label="Steps"></textarea></label>
<script>
// What a page hears from a person, as "field:type:what", kept for the test.
window.heard = [];
function hear(element, name) {
  for (const type of ['beforeinput', 'input', 'keydown'])
    element.addEventListener(type, e => heard.push(name + ':' + type + ':' +
      (e.inputType || e.key || '') + (typeof e.data === 'string' ? '=' + e.data : '')));
}
hear(document.getElementById('note'), 'note');
hear(document.getElementById('steps'), 'steps');

// An editor whose own document is the truth: it rebuilds from beforeinput,
// as a page that never reads the DOM it is drawn into does.
const draftEl = document.getElementById('draft');
let draft = '';
function drawDraft() {
  draftEl.innerHTML = draft.split('\n').map(line => '<p>' + (line || '<br>') + '</p>').join('');
  const range = document.createRange(); range.selectNodeContents(draftEl); range.collapse(false);
  const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range);
}
draftEl.addEventListener('beforeinput', e => {
  if (!e.cancelable) return;
  if (e.inputType === 'insertText' && typeof e.data === 'string') { e.preventDefault(); draft += e.data; drawDraft(); }
  else if (e.inputType === 'insertParagraph' || e.inputType === 'insertLineBreak') { e.preventDefault(); draft += '\n'; drawDraft(); }
});
hear(draftEl, 'draft');

// A block editor: each typed line becomes a block of its own.
const reportEl = document.getElementById('report');
const blocks = [];
function drawReport() {
  reportEl.innerHTML = blocks.map(b => '<div>' + (b || '<br>') + '</div>').join('') + '<div><br></div>';
  const range = document.createRange(); range.selectNodeContents(reportEl.lastElementChild); range.collapse(true);
  const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range);
}
drawReport();
reportEl.addEventListener('keydown', e => {
  if (e.key !== 'Enter' || e.isComposing) return;
  e.preventDefault();
  blocks.push(reportEl.textContent); reportEl.textContent = ''; drawReport();
});
hear(reportEl, 'report');
</script>
'''

SCROLL = b'''<!doctype html><title>Long conditions</title>
<style>p{margin:0 0 200px} #lazy{display:block;min-height:200px}
.invoices{height:124px;overflow:auto} .invoices div{height:40px}</style>
<h1>Conditions of carriage</h1>
<button onclick="this.textContent = 'Terms shown'">Show terms</button>
<p id="first">Condition 1. Tickets are not transferable.</p>
<div style="height:1200px">space the page needs before it loads the rest</div>
<section id="lazy"><p>Nothing here yet.</p></section>
<div id="rows" role="region" aria-label="Invoices" class="invoices" tabindex="0"></div>
<script>
// A section that loads once the page is scrolled to it, as a lazy article
// below the fold does.
new IntersectionObserver(entries => {
  for (const entry of entries) if (entry.isIntersecting && !document.getElementById('conditions'))
    document.getElementById('lazy').insertAdjacentHTML('beforeend',
      '<p id="conditions">Condition 14. Bicycles travel at the owner&rsquo;s risk.</p>');
}).observe(document.getElementById('lazy'));
// A virtualized table: rows are rendered only around what is scrolled to.
const rows = document.getElementById('rows'), ROWS = 400, HEIGHT = 40;
const spacer = document.createElement('div');
spacer.style.height = ROWS * HEIGHT + 'px';
spacer.style.position = 'relative';
rows.append(spacer);
function render() {
  const start = Math.floor(rows.scrollTop / HEIGHT), end = Math.min(ROWS, start + 6);
  spacer.innerHTML = '';
  for (let i = start; i < end; i++) {
    const row = document.createElement('div');
    row.style.position = 'absolute';
    row.style.top = i * HEIGHT + 'px';
    row.textContent = 'Invoice ' + (i + 1) + (i === 59 ? ' (overdue)' : '');
    spacer.append(row);
  }
}
rows.onscroll = render;
render();
</script>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = urlparse(self.path).path
        body = FORM if path == '/form' else AGENT if path == '/agent' else ARTICLE if path == '/article' \
            else LATER if path == '/later' \
            else GESTURE if path == '/gesture' \
            else CANVAS if path == '/canvas' \
            else GESTURES if path == '/gestures' \
            else FRAMED if path == '/framed' else WIDGET if path == '/widget' \
            else LATE if path == '/late' else WIDGET_LATE if path == '/widget-late' \
            else NESTED if path == '/nested' else OUTER if path == '/outer' \
            else RICH if path == '/rich' \
            else SCROLL if path == '/scroll' \
            else b'{"ok":true}' if path == '/data.json' \
            else f'<title>{path}</title><p>{path}</p>'.encode()
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
        found = re.search(pattern + r'[^\n]*\[ref=(d\d+e\d+)\]', outline) or re.search(
            re.escape(pattern) + r'(?:[^\n]*\n)*?[^\n]*\[ref=(d\d+e\d+)\]', outline)
        self.assertIsNotNone(found, f'{pattern!r} not in outline:\n{outline}')
        return found.group(1)

    def _disabled(self, outline, pattern):
        for line in outline.splitlines():
            found = re.search(r'\[ref=(d\d+e\d+)\]', line) if re.search(pattern, line) else None
            if found:
                return found.group(1)
        self.fail(f'{pattern!r} not in outline:\n{outline}')

    def near(self, pattern):
        # A block holding text is not focusable, so a text line carries no ref:
        # what a caller scrolls to is the region around it.
        lines = self.outline().splitlines()
        index = [i for i, text in enumerate(lines) if re.search(pattern, text)]
        self.assertTrue(index, f'{pattern!r} not in outline:\n' + '\n'.join(lines))
        regions = [line for line in lines[:index[0] + 1] if re.search(r'\[ref=d\d+e\d+\]', line)]
        self.assertTrue(regions, f'no region before {pattern!r} in outline:\n' + '\n'.join(lines))
        return re.search(r'\[ref=(d\d+e\d+)\]', regions[-1]).group(1)

    def main_world(self, expression):
        # An answer is known by its own nonce: a title the page wrote for an
        # earlier question may still be on its way.
        nonce = str(time.monotonic_ns())
        self.h.send(op='scripts', scripts=['document.title=JSON.stringify({value:(' + expression + '),nonce:' + nonce + '})'])
        answered = self.h.until(lambda e: e['event'] == 'title' and self._answer(e['title']) == nonce)
        self.h.send(op='scripts', scripts=[])
        return json.loads(re.sub(r',"nonce":\d+}$', '}', answered['title']))['value']

    def _blocks(self, element):
        # What a region holds, a line to each block: an offscreen page lays
        # nothing out, so innerText would run the blocks into one.
        return json.loads(self.main_world(
            'JSON.stringify(Array.from(document.getElementById("%s").children)'
            '.map(b => b.textContent).filter(t => t))' % element))

    @staticmethod
    def _answer(title):
        # The nonce of a value the page wrote into its title, if it is one:
        # the text it carries may hold what JSON would escape.
        found = re.search(r'"nonce":(\d+)}$', title) if title.startswith('{') else None
        return found.group(1) if found else None

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

    def test_a_frame_is_named_by_the_outline_and_operated_in_its_own_document(self):
        self.h.navigate(self.url + '/framed')
        outline = self.outline()
        # The frame is one line of the page, naming the id an action takes to
        # run in it and the address it would reach.
        self.assertRegex(outline, r'iframe "Payment form" \[frame=1\] src="http://127\.0\.0\.1:\d+/widget"')
        # Inside the frame the outline is the frame's own document, and the
        # page around it is not part of it.
        inside = self.ok('outline', frame='1')
        self.assertRegex(inside['outline'], r'textbox "IBAN" \[ref=d\d+e\d+\]')
        self.assertNotIn('Outside the frame', inside['outline'])
        iban = self.ref(inside['outline'], 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000', frame='1')
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame='1')['outline'])
        # A frame's element belongs to the frame: the main document cannot name it.
        wrong = self.page('click', element=iban)
        self.assertEqual((wrong['ok'], wrong['code']), (False, 'stale_element'), wrong)
        # A frame id the outline never printed is refused, not read as the page.
        invented = self.page('outline', frame='7')
        self.assertEqual((invented['ok'], invented['code']), (False, 'no_frame'), invented)

    def test_a_frame_of_a_frame_is_numbered_inside_the_frame_that_holds_it(self):
        self.h.navigate(self.url + '/nested')
        # Each document numbers the frames it holds, so the page's frame is 1
        # and the frame inside it is 1.1 wherever it sits in the page's numbering.
        self.assertRegex(self.outline(), r'iframe "Outer frame" \[frame=1\]')
        outer = self.ok('outline', frame='1')['outline']
        self.assertNotIn('Nested frames', outer, 'the frame reads its own document alone')
        self.assertRegex(outer, r'iframe "Payment form" \[frame=1\]')
        # A frame of a frame is named by both numbers, joined: the same id the
        # page printed to reach its own frame, then the one inside it.
        inner = self.ok('outline', frame='1.1')['outline']
        self.assertRegex(inner, r'textbox "IBAN" \[ref=d\d+e\d+\]')
        self.assertNotIn('Outer document', inner, 'the inner frame reads its own document alone')
        iban = self.ref(inner, 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000', frame='1.1')
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame='1.1')['outline'])
        # The outer frame holds no IBAN of its own: the dotted id is the address.
        held = self.page('fill', element=iban, text='EX11', frame='1')
        self.assertEqual((held['ok'], held['code']), (False, 'stale_element'), held)

    def test_a_frame_the_page_adds_later_is_named_and_operated_in(self):
        self.h.navigate(self.url + '/late')
        # The frame is made by script once the page is up, so it is in no frame
        # tree a read of the page already made; its own document then moves on
        # to the form by itself. Every read of the page from here on must print
        # an id an action can run in, and that frame must be reachable by it.
        deadline = time.monotonic() + HANG_BOUND
        while 'iframe' not in (outline := self.outline()):
            self.assertLess(time.monotonic(), deadline, 'the late frame never appeared')
            time.sleep(0.2)
        numbers = re.findall(r'iframe "Late payment form" \[frame=([\d.]+)\]', outline)
        self.assertEqual(len(numbers), 1, outline)
        frame = numbers[0]
        # The frame's first document leaves for the form by itself, and the read
        # follows it: the id names the frame, never one document of it.
        while 'IBAN' not in (inside := self.ok('outline', frame=frame))['outline']:
            self.assertLess(time.monotonic(), deadline, 'the frame never reached its form')
            time.sleep(0.1)
        self.assertRegex(inside['outline'], r'textbox "IBAN" \[ref=d\d+e\d+\]')
        iban = self.ref(inside['outline'], 'textbox "IBAN"')
        self.ok('fill', element=iban, text='EX89370400440532013000', frame=frame)
        self.assertIn('value="EX89370400440532013000"', self.ok('outline', frame=frame)['outline'])
        holder = self.ref(inside['outline'], 'textbox "Account holder"')
        self.ok('fill', element=holder, text='Maria Fischer', frame=frame)
        save = self.ref(inside['outline'], 'button "Save bank details"')
        self.ok('click', element=save, frame=frame)
        self.assertNotIn('Save bank details', self.ok('outline', frame=frame)['outline'])

    def test_a_click_inside_a_frame_is_a_click_in_the_page(self):
        self.h.navigate(self.url + '/framed')
        inside = self.ok('outline', frame='1')
        button = self.ref(inside['outline'], 'button "Save bank details"')
        self.ok('click', element=button, frame='1')
        # The frame's widget submitted the form: reading the frame again shows
        # the page it reached. The page around it did not move, so nothing is
        # reported as a page navigation.
        self.assertNotIn('Save bank details', self.ok('outline', frame='1')['outline'])

    def test_a_pointer_step_in_a_frame_lands_where_the_frame_sits(self):
        self.h.navigate(self.url + '/framed')
        inside = self.ok('outline', frame='1')
        pad = self.ref(inside['outline'], 'canvas')
        # A step naming an element of a frame is placed by the box the frame
        # gives it, moved by where the frame sits in the page, as a click in
        # that frame is: the press reaches the frame's own element, never the
        # page above it.
        self.pointer([dict(kind='move', element=pad, frame='1', x=5, y=5),
                      dict(kind='down', element=pad, frame='1', x=5, y=5),
                      dict(kind='up', element=pad, frame='1', x=5, y=5)])
        pressed = self.main_world('(frame => frame && frame.contentDocument'
                                  '.getElementById("pressed").textContent)'
                                  '(document.querySelector("iframe"))')
        # The canvas's own box starts 8 by 29 into the frame's document, which
        # starts at the frame's own client origin, so a point 5 by 5 into the
        # canvas is 13 by 34 of the view, as the frame's own element reads it.
        self.assertEqual(pressed, 'pressed 13,34', 'the press reached the frame\'s own element')
        wrong = self.page('pointer', button='left', steps=[dict(kind='down', element=pad, frame='2', x=5, y=5)])
        self.assertEqual((wrong['ok'], wrong['code']), (False, 'no_frame'), wrong)

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
        self.assertEqual((pressed['navigation'], pressed['navigated'], pressed['loading']), ('started', True, False), pressed)
        self.assertTrue(any(e['event'] == 'loaded' and '/done' in e['url'] for e in self.h.seen))
        query = parse_qs(urlparse(pressed['url']).query)
        self.assertEqual((query['email'], query['q']), (['ada@example.test'], ['tea']))
        stale = self.page('click', element=search)
        self.assertEqual((stale['ok'], stale['code']), (False, 'stale_element'), stale)

    def test_fill_a_region_writes_the_text_as_a_person_types_it(self):
        self.h.navigate(self.url + '/rich')
        outline = self.outline()
        filled = self.ok('fill', element=self.ref(outline, 'textbox "Note"'), text='The rent is going up\nfrom October.')
        self.assertTrue(filled['ok'], filled)
        self.assertEqual(self.main_world('document.getElementById("note").textContent'),
                         'The rent is going upfrom October.')
        self.assertEqual(self.main_world('document.getElementById("note").children[0].textContent'),
                         'from October.')
        # The page heard each key, and heard the edit before it was made.
        heard = self.main_world("window.heard.filter(e => e.startsWith('note:'))")
        self.assertIn('note:keydown:Enter', heard)
        self.assertIn('note:beforeinput:insertParagraph', heard)
        typed = [e.split('=', 1)[1] if '=' in e else '\n' for e in heard
                 if e.startswith('note:beforeinput:')]
        self.assertEqual(''.join(typed), 'The rent is going up\nfrom October.', heard[:6])

    def test_fill_reaches_editors_with_a_model_and_blocks_of_their_own(self):
        self.h.navigate(self.url + '/rich')
        outline = self.outline()
        self.ok('fill', element=self.ref(outline, 'textbox "Draft"'), text='First line\nSecond line')
        self.ok('fill', element=self.ref(outline, 'textbox "Report"'), text='What I did\nWhat I will do')
        self.assertEqual(self._blocks('draft'), ['First line', 'Second line'])
        heard = self.main_world("window.heard.filter(e => e.startsWith('draft:'))")
        self.assertTrue(any(e.startswith('draft:beforeinput:insertText') for e in heard), heard[:4])
        self.assertIn('draft:beforeinput:insertParagraph', heard)
        # A block editor heard a real Enter, and started a block for each line.
        self.assertEqual(self._blocks('report'), ['What I did', 'What I will do'])
        self.assertIn('report:keydown:Enter', self.main_world(
            "window.heard.filter(e => e.startsWith('report:'))"))

    def test_enter_in_a_textarea_starts_a_new_line(self):
        self.h.navigate(self.url + '/rich')
        outline = self.outline()
        steps = self.ref(outline, 'textbox "Steps"')
        self.ok('fill', element=steps, text='Sand the frame\nApply two coats')
        # A press after the fill adds a line rather than submitting anything.
        pressed = self.ok('press', element=steps, key='Enter', ctrl=False, shift=False, alt=False, meta=False)
        self.assertFalse(pressed['navigated'], pressed)
        self.ok('fill', element=steps, text='Sand the frame\nApply two coats\nReassemble')
        self.assertEqual(self.main_world('document.getElementById("steps").value'),
                         'Sand the frame\nApply two coats\nReassemble')
        self.assertIn('steps:keydown:Enter', self.main_world(
            "window.heard.filter(e => e.startsWith('steps:'))"))

    def test_clicking_a_link_reports_the_navigation_it_started(self):
        self.h.navigate(self.url + '/form')
        read = self.ok('outline')
        self.assertEqual((read['navigation'], read['navigated']), ('none', False), read)
        link = self.ref(read['outline'], 'link "Next page"')
        clicked = self.ok('click', element=link)
        self.assertEqual((clicked['navigation'], clicked['navigated'], clicked['loading']), ('started', True, False), clicked)
        self.assertTrue(clicked['url'].endswith('/next'), clicked)
        self.assertIn('text "/next"', self.outline())
        idle = self.ok('press', key='Tab', ctrl=False, shift=False, alt=False, meta=False)
        self.assertEqual((idle['navigation'], idle['navigated']), ('possible', False), idle)

    def test_a_navigation_the_page_starts_later_is_its_own_event(self):
        self.h.navigate(self.url + '/later')
        button = self.ref(self.outline(), 'button "Go later"')
        clicked = self.ok('click', element=button)
        # The page navigates a second after it handled the click: the reply
        # cannot know that yet, and says so.
        self.assertEqual((clicked['navigation'], clicked['navigated']), ('possible', False), clicked)
        self.assertTrue(clicked['url'].endswith('/later'), clicked)
        self.h.until(lambda e: e['event'] == 'loaded' and e['url'].endswith('/next'))
        entries = self.ok('network', since=0)['data']['entries']
        later = [entry for entry in entries if entry.get('navigation') and entry['url'].endswith('/next')]
        self.assertEqual(len(later), 1, entries)
        # Chromium attributes it to the click's user activation.
        self.assertTrue(later[0]['user_gesture'], later)


    def test_changes_report_only_what_appeared_after_the_page_settles(self):
        self.h.navigate(self.url + '/agent')
        add = self.ref(self.outline(), 'button "Add later"')
        self.ok('click', element=add)
        # The page adds its line a second after the click; settling waits for it.
        settled = self.ok('settle', quiet_ms=1500, timeout_ms=SETTLE_MS)
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

    def test_a_hover_reveals_a_menu_and_a_click_then_opens_it(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        self.assertNotIn('Fishing licence', outline, 'the menu starts closed')
        # A trigger a hover reveals something under is named by ref: the rule
        # that opens the menu is what a gesture has to address.
        trigger = self.ref(outline, '(?:hint|hoverable) "Licences menu"')
        hovered = self.ok('hover', element=trigger)
        self.assertTrue(hovered['data']['changed'], hovered)
        self.assertIn('+ - list', hovered['outline'], hovered)
        self.assertIn('Fishing licence', self.outline())
        self.ok('click', element=self.ref(self.outline(), 'link "Fishing licence"'))
        self.assertIn('text "opened fishing"', self.outline())
        # A hover keeps the button up: it reveals a menu, it does not press.
        self.assertEqual(self.main_world('drawn()'), 0)

    def test_a_drag_reorders_a_draggable_list(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        first = self.ref(outline, 'draggable "Riverton office"')
        third = self.ref(outline, 'draggable "Central office"')
        dragged = self.ok('drag', element=first, to=third, steps=6)
        self.assertEqual(self.main_world('order()'), 'Porthaven office > Central office > Riverton office', dragged)
        self.assertIn('Dragged', dragged['message'], dragged)

    def test_a_path_draws_on_a_canvas_whose_page_starts_the_line(self):
        # A pad that begins its stroke on pointerdown only draws if the page is
        # the one the pointer is working: an unfocused view drops the moves.
        self.h.navigate(self.url + '/canvas')
        canvas = self.ref(self.outline(), 'canvas "Signature pad"')
        points = [[50, 100], [80, 40], [110, 110], [140, 45], [170, 105], [200, 60], [240, 100]]
        drawn = self.ok('path', element=canvas, points=points)
        self.assertEqual(self.main_world('strokes()'), len(points), drawn)

    def test_a_click_still_lands_after_a_drag(self):
        # A drag leaves the engine's own drag behind it; the click that follows
        # has to reach the page as any click does.
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        first = self.ref(outline, 'draggable "Riverton office"')
        third = self.ref(outline, 'draggable "Central office"')
        self.ok('drag', element=first, to=third, steps=6)
        save = self.ref(self.outline(), 'button "Save order"')
        clicked = self.ok('click', element=save)
        self.assertEqual(self.main_world("document.getElementById('m').textContent"),
                         'saved Porthaven office > Central office > Riverton office', clicked)

    def test_a_pointer_path_draws_on_a_canvas(self):
        self.h.navigate(self.url + '/gestures')
        outline = self.outline()
        self.assertIn('canvas "Signature pad" 400x150', outline)
        pad = self.ref(outline, 'canvas "Signature pad"')
        points = [[20, 110], [40, 60], [60, 100], [80, 50], [100, 105], [120, 70], [140, 95]]
        drawn = self.ok('path', element=pad, points=points)
        # Every point after the press is a move the page strokes; the reply
        # names how many it drew so an agent can tell ink landed.
        self.assertEqual(drawn['data']['drew'], len(points), drawn)
        self.assertEqual(self.main_world('drawn()'), len(points), drawn)
        self.ok('click', element=self.ref(self.outline(), 'button "Submit signature"'))
        self.assertIn('text "Signature received"', self.outline())

    def test_a_capture_paints_what_the_page_only_shows(self):
        # A page can say something in pixels that no outline can carry: a
        # chart, a scanned letter, a code drawn rather than written. The
        # capture is the engine painting the page itself, so what comes back
        # is what a person would see there.
        self.h.navigate(self.url + '/canvas')
        view = self.ok('capture')
        picture = base64.b64decode(view['data']['capture'], validate=True)
        self.assertEqual(view['data']['format'], 'png')
        self.assertTrue(picture.startswith(b'\x89PNG\r\n\x1a\n'), view['data'])
        # The viewport, as the window was told it: the picture is the whole
        # of what is on screen, and no wider than any capture may be.
        self.assertEqual((view['data']['x'], view['data']['y']), (0, 0))
        self.assertEqual(view['data']['width'], 1000)
        self.assertEqual(view['data']['height'], 800)

        # An element's own box, in the page's coordinates: what the outline
        # named, painted where it sits, not the whole screen around it.
        canvas = self.ref(self.outline(), 'canvas "Signature pad"')
        box = self.ok('capture', element=canvas)
        self.assertEqual(box['data']['format'], 'png')
        # The pad's own 500x150 canvas, with the border a person sees around
        # it: the box is the element's, borders and all.
        self.assertEqual(box['data']['width'], 502)
        self.assertEqual(box['data']['height'], 152)
        painted = base64.b64decode(box['data']['capture'], validate=True)
        self.assertTrue(painted.startswith(b'\x89PNG\r\n\x1a\n'), box['data'])
        self.assertIn('Signature pad', box['message'])

        # A caller who asks for a narrower picture gets one: the page is
        # scaled, never cut, and never widened.
        narrow = self.ok('capture', element=canvas, max_width=200)
        self.assertEqual(narrow['data']['width'], 200)
        self.assertEqual(narrow['data']['height'], round(152 * 200 / 502))

        # A ref the outline never printed names nothing to picture, and is
        # refused before the page is asked to paint anything.
        refused = self.page('capture', element='not-a-ref')
        self.assertFalse(refused['ok'], refused)
        self.assertEqual(refused['code'], 'invalid_request')
        self.assertNotIn('capture', refused)

        # A click that lands on the pad is drawn ink: the capture is of the
        # page as it is now, not as it was loaded.
        points = [[40, 60], [90, 40], [140, 100]]
        self.ok('path', element=canvas, points=points)
        ink = self.ok('capture', element=canvas)
        self.assertNotEqual(ink['data']['capture'], box['data']['capture'],
                            'a picture is painted when it is asked for')

    def test_console_and_network_reach_the_agent_and_not_the_page(self):
        self.h.navigate(self.url + '/agent')
        log = self.ref(self.outline(), 'button "Log"')
        self.ok('click', element=log)
        self.ok('settle', quiet_ms=300, timeout_ms=SETTLE_MS)
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

    def test_file_inputs_show_themselves_and_take_chosen_files(self):
        self.h.navigate(self.url + '/form')
        outline = self.outline()
        self.assertRegex(outline, r'file "Proof of address" \[ref=d\d+e\d+\] type=file accept="\.pdf" files=none')
        self.assertRegex(outline, r'file "Receipts" \[ref=d\d+e\d+\] type=file multiple files=none')
        receipts = self.ref(outline, 'file "Receipts"')
        proof = self.ref(outline, 'file "Proof of address"')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'proof.pdf')
            second = os.path.join(folder, 'one.png')
            third = os.path.join(folder, 'two.png')
            for path, body in ((first, b'%PDF-1.4'), (second, b'\x89PNG one'), (third, b'\x89PNG two')):
                with open(path, 'wb') as file:
                    file.write(body)
            # The element's own rule is the page's rule: a script cannot pick
            # a file the page would have refused, nor a text field at all.
            refused = self.page('fill_files', element=proof, paths=[second])
            self.assertEqual((refused['ok'], refused['code']), (False, 'not_accepted'), refused)
            text = self.ref(outline, 'textbox "Email"')
            refused = self.page('fill_files', element=text, paths=[first])
            self.assertFalse(refused['ok'], refused)
            self.assertIn(text, refused['message'])
            chosen = self.ok('fill_files', element=proof, paths=[first])
            self.assertEqual(chosen['data']['files'], ['proof.pdf'], chosen)
            # The page receives the picker's own trusted events.
            self.assertEqual(self.main_world('document.getElementById("status").textContent'),
                             'files proof.pdf:8')
            self.assertRegex(self.outline(), r'file "Proof of address" .*files="proof.pdf"')
            chosen = self.ok('fill_files', element=receipts, paths=[second, third])
            self.assertEqual(chosen['data']['files'], ['one.png', 'two.png'], chosen)
            self.assertEqual(self.main_world('document.getElementById("status").textContent'),
                             'files one.png:8,two.png:8')
            # A click on a file input asks the page's own chooser for files;
            # nothing is chosen for it, so the outline stays as it was.
            clicked = self.page('click', element=receipts)
            self.assertTrue(clicked['ok'], clicked)

    def test_chosen_files_reach_the_page_with_and_without_a_held_chooser(self):
        # Choosing files names the element in the inspected document, not in a
        # node id: a ref read from the outline resolves there, and the page
        # receives the picker's own input and change events either way.
        self.h.navigate(self.url + '/form')
        with tempfile.TemporaryDirectory() as folder:
            first = os.path.join(folder, 'proof.pdf')
            second = os.path.join(folder, 'second.pdf')
            for path, body in ((first, b'%PDF-1.4'), (second, b'%PDF-1.5')):
                with open(path, 'wb') as file:
                    file.write(body)
            proof = self.ref(self.outline(), 'file "Proof of address"')
            chosen = self.ok('fill_files', element=proof, paths=[first])
            self.assertEqual(chosen['data']['files'], ['proof.pdf'], chosen)
            self.assertEqual(self.main_world('document.querySelector("[name=proof]").files[0].name'),
                             'proof.pdf')
            self.assertEqual(self.main_world('document.getElementById("status").textContent'),
                             'files proof.pdf:8')
            # A click holds the page's chooser; answering it with the same
            # action reaches the same field, so the page sees the second file.
            clicked = self.ok('click', element=proof)
            self.assertIn('file chooser', clicked['message'], clicked)
            chosen = self.ok('fill_files', element=proof, paths=[second])
            self.assertEqual(chosen['data']['files'], ['second.pdf'], chosen)
            self.assertEqual(self.main_world('document.querySelector("[name=proof]").files[0].name'),
                             'second.pdf')

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

    def pointer(self, steps, button='left'):
        return self.ok('pointer', steps=steps, button=button)

    def test_pointer_hover_reveals_a_menu_and_the_click_opens_it(self):
        self.h.navigate(self.url + '/gesture')
        outline = self.outline()
        self.assertNotIn('Fishing licence', outline, 'the menu is closed until a hand rests on it')
        # The page keeps the menu shut until the pointer rests on its owner,
        # so the outline names that owner and a gesture reaches it by ref.
        resting = self.ref(outline, 'hoverable "Licences"')
        self.pointer([dict(kind='move', element=resting, x=5, y=5)])
        revealed = self.outline()
        fishing = self.ref(revealed, 'link "Fishing licence"')
        self.pointer([dict(kind='move', element=fishing, x=5, y=5),
                      dict(kind='down', element=fishing, x=5, y=5),
                      dict(kind='up', element=fishing, x=5, y=5)])
        self.assertEqual(self.main_world('location.pathname'), '/fishing', 'the click followed the link')

    def test_pointer_drags_a_page_element_onto_another(self):
        self.h.navigate(self.url + '/gesture')
        outline = self.outline()
        central = self.ref(outline, 'draggable "Central office"')
        riverton = self.ref(outline, 'draggable "Riverton office"')
        self.pointer([dict(kind='down', element=central, x=5, y=5),
                      dict(kind='move', element=riverton, x=5, y=5),
                      dict(kind='up', element=riverton, x=5, y=5)])
        order = self.main_world("[...document.querySelectorAll('#rank li')].map(li => li.textContent).join(' > ')")
        self.assertEqual(order, 'Central office > Riverton office > Porthaven office', order)

    def test_pointer_draws_on_a_canvas(self):
        self.h.navigate(self.url + '/gesture')
        pad = self.ref(self.outline(), 'canvas "Signature pad"')
        steps = []
        for line in range(3):
            steps.append(dict(kind='down', element=pad, x=20, y=20 + line * 20))
            steps.append(dict(kind='move', element=pad, x=120, y=20 + line * 20))
            steps.append(dict(kind='up', element=pad, x=120, y=20 + line * 20))
        self.pointer(steps)
        self.assertIn('text "strokes 3"', self.outline(), 'each press and release is one stroke')

    def test_scrolling_to_a_lazy_section_loads_it(self):
        self.h.navigate(self.url + '/scroll')
        self.assertNotIn('Condition 14', self.outline(), 'the section loads only once it is scrolled to')
        # The wheel loads the section below the fold as a hand's scroll would:
        # the section's observer runs when the turn puts it on screen, and the
        # page it loads goes on to extend the page, which the wheel follows.
        self.ok("scroll", by=1000)
        self.assertIn('Condition 14', self.outline(), 'the wheel loaded the section it scrolled to')
        self.assertTrue(self.main_world('scrollY > 0'), 'the page itself moved')

    def test_scrolling_back_up_returns_to_the_top(self):
        self.h.navigate(self.url + '/scroll')
        self.ok('scroll', to='bottom')
        self.assertTrue(self.main_world('scrollY > 0'))
        self.ok('scroll', to='top')
        self.assertEqual(self.main_world('scrollY'), 0)

    def test_a_virtualized_table_renders_the_rows_scrolled_to(self):
        self.h.navigate(self.url + '/scroll')
        self.assertIn('Invoice 6', self.main_world("[...document.querySelectorAll('#rows div')].map(d => d.textContent).join(', ')"))
        # Rows are rendered as the list is scrolled, and its own scroll
        # listener renders the rows further on as the wheel reaches them.
        invoices = self.ref(self.outline(), 'region "Invoices"')
        self.ok('scroll', to='bottom')
        scrolled = self.main_world('scrollY')
        # The wheel turns over the list until Invoice 60 comes into view.
        self.ok('scroll', element=invoices, by=2300)
        self.assertIn('Invoice 60 (overdue)', self.outline(), 'the rows the wheel reached are rendered')
        self.assertEqual(self.main_world('scrollY'), scrolled, 'the list scrolled, not the page')

    def test_an_element_that_cannot_hear_a_scroll_is_refused(self):
        self.h.navigate(self.url + '/scroll')
        # The list's box is below the fold, so its own wheel cannot turn yet:
        # the request says so instead of turning the page's wheel instead.
        invoices = self.ref(self.outline(), 'region "Invoices"')
        refused = self.page('scroll', element=invoices, by=400)
        self.assertEqual((refused['ok'], refused['code']), (False, 'not_visible'), refused)
        self.assertEqual(self.main_world("document.getElementById('rows').scrollTop"), 0)

    def test_a_scroll_that_says_nothing_or_two_things_is_refused(self):
        self.h.navigate(self.url + '/scroll')
        # Where one scroll goes is said once, and what is outside the bounds a
        # scroll may ask for is answered rather than done. A refusal answers in
        # its turn, so the keys of a scroll are only the ones that make one:
        # what is not refused here is a scroll the page itself turns down.
        for fields in ({}, {'to': 'sideways'}, {'to': 'top', 'by': 100},
                       {'into_view': 'd1e2', 'by': 100}, {'by': 20000}):
            refused = self.page('scroll', **fields)
            self.assertEqual((refused['ok'], refused['code']), (False, 'invalid_request'),
                             (fields, refused))
            self.assertEqual(self.main_world('scrollY'), 0, f'{fields} moved the page')
        self.ok('scroll', by=200)
        self.assertTrue(self.main_world('scrollY') > 0, 'a scroll after a refusal still runs')

    def test_an_element_is_brought_into_view(self):
        self.h.navigate(self.url + '/scroll')
        # A row the page has rendered but the list clips is brought into view
        # by the same wheel a person turns over it.
        invoices = self.ref(self.outline(), 'region "Invoices"')
        self.ok('scroll', to='bottom')
        # The wheel turns over the list until Invoice 60 comes into view.
        self.ok('scroll', element=invoices, by=2300)
        later = self.near('Invoice 6[0-9]')
        self.ok('scroll', into_view=later)
        self.assertIn('Invoice 60 (overdue)', self.outline(), 'the wheel brought the row into view')

    def test_a_scroll_past_a_focused_control_moves_the_page(self):
        self.h.navigate(self.url + '/scroll')
        # A click focuses a control, and from then on the page's own focused
        # element eats End and PageDown; the wheel does not care who has the
        # keyboard.
        button = self.ref(self.outline(), 'button "Show terms"')
        self.ok('click', element=button)
        self.ok('scroll', by=600)
        self.assertGreater(self.main_world('scrollY'), 0)

    def test_a_scroll_that_says_nothing_bounded_is_refused(self):
        self.h.navigate(self.url + '/scroll')
        # A distance is bounded, an end of the page is one of two names, and a
        # scroll says where it goes once; a page request outside those bounds
        # is answered as failed, not run.
        for fields in (dict(by=20000), dict(to='sideways'), dict(to='top', by=100), dict()):
            result = self.page('scroll', **fields)
            self.assertFalse(result['ok'], result)
        self.assertEqual(self.main_world('scrollY'), 0, 'the wheel moved the page')



if __name__ == '__main__':
    unittest.main(verbosity=2)
