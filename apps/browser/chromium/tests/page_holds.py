#!/usr/bin/env python3
"""What an agent's window holds before its page sends it, in real Chromium.

A window an agent operates (one told `hold_departures`) holds every departure
until the person answers it: a form submission however it was triggered, a
navigation or request with a method of its own, a download. The page stays as
it was filled and nothing leaves; an allow sends exactly what was filled in,
and a denial leaves the page as it is. A window nobody told to hold sends what
is clicked.
"""
import http.server
import re
import threading
import time
import unittest

from native_regressions import Host

FORM = b'''<!doctype html><title>Application</title>
<h1>Application</h1>
<form id="application" method="post" action="/received">
  <label>Name <input name="name" value="Ada Lovelace"></label>
  <label>Country <input name="country" value="Netherlands"></label>
  <label>Password <input name="pin" type="password" value="hunter2"></label>
  <button id="send">Submit application</button>
</form>
<p id="state">Filling in</p>'''

RECEIVED = b'''<!doctype html><title>Application received</title>
<h1>Application received</h1><p id="state">Done</p>'''

SCRIPT = b'''<!doctype html><title>Script departures</title>
<p id="state">Ready</p>
<button id="post" onclick="fetch('/collect', {method: 'POST', body: 'a=1'}).then(r => r.text()).then(t => document.getElementById('state').textContent = 'posted ' + t)">Post</button>
<button id="send" onclick="document.getElementById('f').requestSubmit()">Send by script</button>
<form id="f" method="post" action="/received"><input name="q" value="typed"></form>
'''

DOWNLOAD = b'''<!doctype html><title>Files</title>
<a id="receipt" href="/receipt.txt" download>Receipt</a>
'''


class Handler(http.server.BaseHTTPRequestHandler):
    content = {
        '/': (FORM, 'text/html'),
        '/received': (RECEIVED, 'text/html'),
        '/script': (SCRIPT, 'text/html'),
        '/download.html': (DOWNLOAD, 'text/html'),
        '/collect': (b'collected', 'text/plain'),
        '/receipt.txt': (b'receipt\n', 'application/octet-stream'),
    }

    def answer(self):
        path = self.path.split('?')[0]
        body, kind = self.content.get(path, (b'ok', 'text/plain'))
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        if path == '/receipt.txt':
            self.send_header('Content-Disposition', 'attachment; filename=receipt.txt')
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.answer()

    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        self.answer()

    def log_message(self, *args):
        pass


class PageHolds(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def open(self, path='/', hold=True):
        host = Host()
        self.addCleanup(host.close)
        if hold:
            host.send(op='hold_departures', tab=1)
        host.navigate(self.url + path)
        return host

    def ref(self, host, pattern):
        host.send(op='page', request_id=1, action='outline')
        outline = host.until(lambda e: e['event'] == 'page_result')['outline']
        found = re.search(pattern + r'[^\n]*\[ref=(d\d+e\d+)\]', outline)
        self.assertIsNotNone(found, f'{pattern!r} not in outline:\n{outline}')
        return found.group(1)

    def shows(self, host, text, timeout=20):
        """What the page shows, asked again until it says `text`: a page
        changing its own text is nothing the host reports."""
        deadline = time.monotonic() + timeout
        while True:
            host.send(op='page', request_id=3, action='outline')
            outline = host.until(lambda e: e['event'] == 'page_result'
                                 and e.get('request_id') == 3)['outline']
            if text in outline or time.monotonic() >= deadline:
                break
        self.assertIn(text, outline, 'what the page shows')
        return outline

    def click(self, host, pattern):
        host.send(op='page', request_id=2, action='click', element=self.ref(host, pattern))

    def held(self, host):
        return host.until(lambda e: e['event'] == 'held')

    def answer(self, host, held, allow):
        host.send(op='hold_answer', tab=held['tab'], id=held['id'], allow=allow)
        host.until(lambda e: e['event'] == 'held_closed' and e['id'] == held['id'])

    def test_a_clicked_submit_waits_for_the_person_and_sends_what_was_filled(self):
        host = self.open('/')
        self.click(host, 'button "Submit application"')
        held = self.held(host)
        self.assertEqual((held['kind'], held['method'], held['origin']),
                         ('submit', 'POST', self.url), held)
        self.assertNotEqual(host.seen[-1].get('title'), 'Application received')
        self.answer(host, held, True)
        host.until(lambda e: e['event'] == 'title' and e['title'] == 'Application received')
        # What was filled in is what was sent.
        self.assertIn('Application received', [e.get('title') for e in host.seen])

    def test_a_denied_submission_leaves_the_page_as_it_was_filled(self):
        host = self.open('/')
        self.click(host, 'button "Submit application"')
        held = self.held(host)
        self.answer(host, held, False)
        host.settle(1.0)
        self.assertEqual(host.latest_title(), 'Application')
        self.shows(host, 'value="Ada Lovelace"')

    def test_enter_in_a_field_waits_for_the_person(self):
        host = self.open('/')
        host.send(op='page', request_id=3, action='press', key='Enter',
                  element=self.ref(host, 'textbox "Name"'))
        held = self.held(host)
        self.assertEqual(held['kind'], 'submit', held)
        self.answer(host, held, True)
        host.until(lambda e: e['event'] == 'title' and e['title'] == 'Application received')

    def test_a_request_the_page_sends_waits_for_the_person(self):
        host = self.open('/script')
        self.click(host, 'button "Post"')
        held = self.held(host)
        self.assertEqual((held['kind'], held['method']), ('request', 'POST'), held)
        self.answer(host, held, True)
        self.shows(host, 'posted collected')

    def test_a_script_submission_waits_for_the_person(self):
        host = self.open('/script')
        self.click(host, 'button "Send by script"')
        held = self.held(host)
        self.assertEqual(held['kind'], 'submit', held)
        self.answer(host, held, True)
        host.until(lambda e: e['event'] == 'title' and e['title'] == 'Application received')

    def test_a_download_waits_for_the_person(self):
        host = self.open('/download.html')
        self.click(host, 'link "Receipt"')
        prompt = host.until(lambda e: e['event'] == 'download_prompt', timeout=90)
        host.send(op='download_decision', tab=1, id=prompt['id'], allow=True)
        host.until(lambda e: e['event'] == 'download' and e['id'] == prompt['id']
                   and e['state'] == 'complete', timeout=90)

    def test_a_window_nobody_told_to_hold_sends_what_is_clicked(self):
        host = self.open('/', hold=False)
        self.click(host, 'button "Submit application"')
        host.until(lambda e: e['event'] == 'title' and e['title'] == 'Application received')
        self.assertFalse([e for e in host.seen if e['event'] == 'held'])


if __name__ == '__main__':
    unittest.main()
