#!/usr/bin/env python3
"""A page's copies reach the Browser, and its pastes come from the Browser.

On Linux the engine runs with no display server, so Chromium's clipboard is
its own alone (src/page_clipboard.h). With real Chromium:

- `ready` says the host bridges the page's clipboard.
- A click on a site's copy button sends what it copied as a `clipboard`
  event: `navigator.clipboard.writeText`, `execCommand('copy')` on a field
  the page selected, and a copy listener's own text and markup.
- What is copied from a password field, or is a password field's value, is
  marked as a secret.
- The person's Ctrl+C sends what they selected.
- A page copies nothing without the person: a copy as it loads sends none.
- Ctrl+V asks the Browser for the system clipboard (`clipboard_read`) and
  pastes its answer, which the page's paste listener reads too; keys pressed
  while it waits reach the page after it, in order.
- `navigator.clipboard.readText` that Chromium refuses reads nothing of the
  system clipboard. (Once the person allows a site, its read gets the system
  clipboard's text; this node refuses a site's read without asking, so that
  half is not covered here.)
- A rich editor's field in a document its page wrote into a frame
  (srcdoc, about:blank) copies to and pastes from the Browser as the page
  does.
- A tab whose embedder did not ask for the page's clipboard sends nothing
  and pastes the engine's own at once.
"""
import http.server
import json
import queue
import threading
import unittest
from native_regressions import HANG_BOUND, Host

PAGE = b'''<!doctype html><title>ready</title><body style="margin:0">
<button id="link" style="position:absolute;left:0;top:0;width:200px;height:80px">Copy link</button>
<button id="exec" style="position:absolute;left:0;top:100px;width:200px;height:80px">Copy text</button>
<button id="rich" style="position:absolute;left:0;top:200px;width:200px;height:80px">Copy formatted</button>
<button id="pw" style="position:absolute;left:0;top:300px;width:200px;height:80px">Copy password</button>
<button id="read" style="position:absolute;left:0;top:400px;width:200px;height:80px">Read</button>
<input id="secret" type="password" value="hunter2-secret" style="position:absolute;left:300px;top:0;width:200px">
<textarea id="field" style="position:absolute;left:300px;top:100px;width:300px;height:80px"></textarea>
<script>
const show = (what) => { document.title = JSON.stringify(what); };
link.onclick = () => navigator.clipboard.writeText('https://example.test/shared').then(() => show('wrote'), (e) => show('refused ' + e));
exec.onclick = () => {
  const t = document.createElement('textarea');
  t.value = 'copied by execCommand';
  document.body.append(t);
  t.select();
  document.execCommand('copy');
  t.remove();
};
rich.onclick = () => {
  document.addEventListener('copy', (e) => {
    e.clipboardData.setData('text/plain', 'plain words');
    e.clipboardData.setData('text/html', '<b>bold words</b>');
    e.preventDefault();
  }, {once: true});
  document.execCommand('copy');
};
pw.onclick = () => navigator.clipboard.writeText(secret.value);
read.onclick = () => navigator.clipboard.readText().then((text) => show({read: text}), (e) => show('refused ' + e.name));
// Each click says it reached the page, for a test that waits on a copy.
for (const button of document.querySelectorAll('button')) button.addEventListener('click', () => show({clicked: button.id}), true);
let pasted = null;
field.addEventListener('paste', (e) => { pasted = e.clipboardData.getData('text/plain'); });
field.addEventListener('input', () => show({value: field.value, pasted}));
</script></body>'''
# A rich editor's field, in a document the page wrote into a frame.
FRAMED = b'''<!doctype html><title>ready</title><body style="margin:0">
<iframe style="position:absolute;left:0;top:0;width:400px;height:200px;border:0" srcdoc="<body style='margin:0'>
<textarea id='field' style='position:absolute;left:0;top:0;width:300px;height:80px'></textarea>
<script>field.addEventListener('input', () => { parent.document.title = JSON.stringify({value: field.value}); });</script>"></iframe>
</body>'''
# A page that copies as it loads, with no one there.
UNASKED = b'''<!doctype html><title>unasked</title><body>words<script>
navigator.clipboard.writeText('unasked').catch(() => {});
getSelection().selectAllChildren(document.body);
document.execCommand('copy');
</script></body>'''


def state(event):
    """What the page shows in its title, as the event carries it: {} for a
    title that is no state."""
    if event['event'] != 'title':
        return {}
    try:
        shown = json.loads(event['title'])
    except ValueError:
        return {}
    return shown if isinstance(shown, dict) else {}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = {'/unasked': UNASKED, '/framed': FRAMED}.get(self.path, PAGE)
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class PageClipboard(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        # localhost is a secure context: the page has navigator.clipboard.
        cls.url = f'http://localhost:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def start(self, share=True, page='/page'):
        host = Host()
        self.addCleanup(host.close)
        ready = next(e for e in host.seen if e['event'] == 'ready')
        self.assertIs(ready.get('page_clipboard'), True, 'the engine has no clipboard of its own here')
        if share:
            host.send(op='page_clipboard')
        host.navigate(self.url + page)
        host.send(op='focus', focused=True)
        return host

    def click(self, host, x, y):
        for down in (True, False):
            host.send(op='mouse_button', button='left', down=down, x=float(x), y=float(y))

    def key(self, host, key, ctrl=False, down=None):
        for each in ((True, False) if down is None else (down,)):
            host.send(op='key', key=key, down=each, ctrl=ctrl, shift=False, alt=False, meta=False)

    def copied(self, host, mark):
        """The first `clipboard` event after `mark`."""
        found = next((e for e in host.seen[mark:] if e['event'] == 'clipboard'), None)
        if found:
            return found
        try:
            return host.until(lambda e: e['event'] == 'clipboard', timeout=min(HANG_BOUND, 20))
        except (TimeoutError, queue.Empty):
            self.fail('no copy reached the host; it said ' + repr([e for e in host.seen[mark:] if e['event'] != 'cursor'][-12:]))

    def test_a_click_on_a_copy_button_reaches_the_browser(self):
        host = self.start()
        for (x, y), text, html, secret in [
            ((100, 40), 'https://example.test/shared', None, False),
            ((100, 140), 'copied by execCommand', None, False),
            ((100, 240), 'plain words', '<b>bold words</b>', False),
            ((100, 340), 'hunter2-secret', None, True),
        ]:
            with self.subTest(text=text):
                mark = len(host.seen)
                self.click(host, x, y)
                host.until(lambda e: 'clicked' in state(e))
                copy = self.copied(host, mark)
                self.assertEqual(copy['tab'], 1)
                self.assertEqual(copy['text'], text)
                self.assertEqual(copy.get('html'), html)
                self.assertIs(copy['secret'], secret)

    def test_the_persons_copy_reaches_the_browser(self):
        host = self.start()
        self.click(host, 450, 140)
        for char in 'typed':
            self.key(host, char)
        host.until(lambda e: state(e).get('value') == 'typed')
        mark = len(host.seen)
        self.key(host, 'a', ctrl=True)
        self.key(host, 'c', ctrl=True)
        copy = self.copied(host, mark)
        self.assertEqual((copy['text'], copy['secret']), ('typed', False))

    def test_a_page_copies_nothing_without_the_person(self):
        host = self.start()
        mark = len(host.seen)
        host.navigate(self.url + '/unasked')
        host.settle(2.0)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'clipboard'])

    def test_a_paste_comes_from_the_system_clipboard(self):
        host = self.start()
        self.click(host, 450, 140)
        mark = len(host.seen)
        self.key(host, 'v', ctrl=True, down=True)
        # Typed while the paste waits: it lands after the pasted words.
        self.key(host, 'v', ctrl=True, down=False)
        self.key(host, '!')
        asked = host.until(lambda e: e['event'] == 'clipboard_read')
        self.assertEqual(asked['tab'], 1)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'title' and e['title'] != 'ready'],
                         'the page gets nothing before the clipboard answers')
        host.send(op='clipboard_contents', request_id=asked['request_id'], text='from elsewhere')
        shown = state(host.until(lambda e: state(e).get('value') == 'from elsewhere!'))
        self.assertEqual(shown['pasted'], 'from elsewhere')

    def test_a_field_in_a_written_frame_copies_and_pastes_through_the_browser(self):
        host = self.start(page='/framed')
        host.settle(1.0)
        self.click(host, 100, 40)
        self.key(host, 'v', ctrl=True)
        asked = host.until(lambda e: e['event'] == 'clipboard_read')
        host.send(op='clipboard_contents', request_id=asked['request_id'], text='from elsewhere')
        host.until(lambda e: state(e).get('value') == 'from elsewhere')
        mark = len(host.seen)
        self.key(host, 'a', ctrl=True)
        self.key(host, 'c', ctrl=True)
        copy = self.copied(host, mark)
        self.assertEqual((copy['tab'], copy['text'], copy['secret']), (1, 'from elsewhere', False))

    def test_a_read_chromium_refuses_asks_nothing_of_the_browser(self):
        # On this node Chromium refuses a site's read without asking, as it
        # refuses notifications and location (see t.toml): the page is told
        # so, and the system clipboard is never read for it.
        host = self.start()
        mark = len(host.seen)
        self.click(host, 100, 440)
        host.until(lambda e: e['event'] == 'title' and e['title'] == '"refused NotAllowedError"')
        host.settle(1.0)
        self.assertFalse([e for e in host.seen[mark:] if e['event'] == 'clipboard_read'])

    def test_a_tab_that_did_not_ask_keeps_the_engines_clipboard(self):
        host = self.start(share=False)
        mark = len(host.seen)
        self.click(host, 100, 40)
        host.until(lambda e: e['event'] == 'title' and e['title'] == '"wrote"')
        self.click(host, 450, 140)
        self.key(host, 'v', ctrl=True)
        # The engine pastes what the page copied into it, at once.
        host.until(lambda e: state(e).get('value') == 'https://example.test/shared', timeout=min(HANG_BOUND, 10))
        self.assertFalse([e for e in host.seen[mark:] if e['event'] in ('clipboard', 'clipboard_read')])


if __name__ == '__main__':
    unittest.main(verbosity=2)
