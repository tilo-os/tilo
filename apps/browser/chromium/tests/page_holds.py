#!/usr/bin/env python3
"""What an agent's window holds before its page sends it, in real Chromium.

A window an agent operates (one told `hold_departures`) holds every departure
until the person answers it: a form submission however it was triggered, a
navigation or request with a method of its own, a download. The page stays as
it was filled and nothing leaves; an allow sends exactly what was filled in,
and a denial leaves the page as it is. A window nobody told to hold sends what
is clicked.

The tab's record says what was held and how it ended: whose action reached it,
who let it leave or kept the page as it is, as the app says with its answer,
and what was dropped with nobody answering. Nothing of what was about to leave
is in it.
"""
import http.server
import json
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

AUTO = b'''<!doctype html><title>Posts on its own</title>
<p id="state">Ready</p>
<script>
const say = text => () => document.getElementById('state').textContent = text
addEventListener('load', () => setTimeout(() => fetch('/collect', {method: 'POST', body: 'card=4000056655665556'}).then(say('sent'), say('not sent')), 400))
</script>
'''

DOWNLOAD = b'''<!doctype html><title>Files</title>
<a id="receipt" href="/receipt.txt" download>Receipt</a>
'''


class Handler(http.server.BaseHTTPRequestHandler):
    content = {
        '/': (FORM, 'text/html'),
        '/received': (RECEIVED, 'text/html'),
        '/script': (SCRIPT, 'text/html'),
        '/auto': (AUTO, 'text/html'),
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
    requests = 0

    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def open(self, path='/', hold=True, wait_ms=None):
        """A window on `path` that holds what its page sends, unless `hold`
        says not. `wait_ms` is how long a hold waits for its answer, when the
        app names a time of its own."""
        host = Host()
        self.addCleanup(host.close)
        if hold:
            host.send(op='hold_departures', tab=1, **({} if wait_ms is None else {'wait_ms': wait_ms}))
        host.navigate(self.url + path)
        return host

    def page(self, host, action, **fields):
        """The answer to one page action, whether it came while the test
        waited for something else or has to be waited for."""
        PageHolds.requests += 1
        request = PageHolds.requests
        host.send(op='page', request_id=request, action=action, **fields)
        match = lambda e: e['event'] == 'page_result' and e.get('request_id') == request
        return next((e for e in host.seen if match(e)), None) or host.until(match)

    def read(self, host):
        """What the page shows. A tab runs one action at a time, so a read
        that arrives while the action before it still runs is refused and
        asked again."""
        deadline = time.monotonic() + 30
        while True:
            answer = self.page(host, 'outline')
            if 'outline' in answer:
                return answer['outline']
            self.assertLess(time.monotonic(), deadline, f'the page never answered the read: {answer}')
            time.sleep(0.1)

    def shows(self, host, text):
        """What the page shows, read again until it says `text`: a page
        changing its own text is nothing the host reports."""
        deadline = time.monotonic() + 30
        while True:
            outline = self.read(host)
            if text in outline:
                return outline
            self.assertLess(time.monotonic(), deadline, f'{text!r} not in what the page shows:\n{outline}')
            time.sleep(0.1)

    def ref(self, host, pattern):
        outline = self.read(host)
        found = re.search(pattern + r'[^\n]*\[ref=(d\d+e\d+)\]', outline)
        self.assertIsNotNone(found, f'{pattern!r} not in outline:\n{outline}')
        return found.group(1)

    def click(self, host, pattern):
        PageHolds.requests += 1
        host.send(op='page', request_id=PageHolds.requests, action='click',
                  element=self.ref(host, pattern))

    def held(self, host):
        return host.until(lambda e: e['event'] == 'held')

    def answer(self, host, held, allow, **who):
        """Answer `held` as the app does. `who` is what it says of the answer:
        the `actor` that gave it, and for `reason` that it `expired`."""
        host.send(op='hold_answer', tab=held['tab'], id=held['id'], allow=allow, **who)
        host.until(lambda e: e['event'] == 'held_closed' and e['id'] == held['id'])

    def heard(self, host, match):
        """The first event `match` names, whether it came while the test
        waited for something else or is still to come."""
        return next((e for e in host.seen if match(e)), None) or host.until(match)

    def set_down(self, host, action):
        """The entry the host says it set down in the tab's record for a
        hold, of its own accord: the one whose `action` is `action`."""
        return self.heard(host, lambda e: e['event'] == 'page_record'
                          and e['entry'].get('how') == 'hold' and e['entry'].get('action') == action)['entry']

    def record(self, host):
        """The tab's record as its engine lists it."""
        return self.page(host, 'ledger', since=0)['data']['entries']

    def told(self, host):
        """Everything the host said of the tab's record, as one text."""
        return json.dumps(self.record(host)) + json.dumps([e for e in host.seen if e['event'] == 'page_record'])

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
        field = self.ref(host, 'textbox "Name"')
        PageHolds.requests += 1
        host.send(op='page', request_id=PageHolds.requests, action='press', key='Enter',
                  element=field, ctrl=False, shift=False, alt=False, meta=False)
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

    def test_a_held_submission_is_set_down_with_whose_action_reached_it_and_who_let_it_leave(self):
        # A reviewer reads in the tab's record that the submission waited for
        # the person, whose action reached it and who let it leave.
        host = self.open('/')
        button = self.ref(host, 'button "Submit application"')
        PageHolds.requests += 1
        host.send(op='page', request_id=PageHolds.requests, action='click', element=button, actor='ada')
        held = self.heard(host, lambda e: e['event'] == 'held')
        hold = self.set_down(host, 'submit')
        self.assertEqual((hold['how'], hold['actor'], hold['ok']), ('hold', 'ada', True), hold)
        # The page it was held on, where it would go and how: no more.
        self.assertEqual((hold['url'], hold['to_url'], hold['message']),
                         (self.url + '/', self.url, 'POST ' + self.url), hold)
        # It takes its place after the action whose run reached it.
        entries = self.record(host)
        self.assertEqual([(e['seq'], e.get('how'), e['action'], e['actor']) for e in entries],
                         [(1, None, 'click', 'ada'), (2, 'hold', 'submit', 'ada')])
        self.assertEqual(entries[1], hold)
        # The engine cannot know who answers: the app says, and it is set down.
        self.answer(host, held, True, actor='rev')
        allowed = self.set_down(host, 'allow')
        self.assertEqual((allowed['seq'], allowed['how'], allowed['actor'], allowed['ok']), (3, 'hold', 'rev', True), allowed)
        self.assertEqual((allowed['to_url'], allowed['message']), (self.url, 'POST ' + self.url), allowed)
        host.until(lambda e: e['event'] == 'title' and e['title'] == 'Application received')
        self.assertEqual([e['action'] for e in self.record(host)], ['click', 'submit', 'allow'])
        # What the form held left with the submission and is nowhere in the
        # record: no field's value, and not the password.
        told = self.told(host)
        for text in ('Ada Lovelace', 'Netherlands', 'hunter2'):
            self.assertNotIn(text, told)

    def test_a_page_kept_as_it_is_is_set_down_as_the_persons_when_the_app_names_nobody(self):
        # An app that says nothing of who answered answers as the person at
        # the window, as every app did before the record kept holds.
        host = self.open('/')
        self.click(host, 'button "Submit application"')
        held = self.heard(host, lambda e: e['event'] == 'held')
        self.answer(host, held, False)
        kept = self.set_down(host, 'keep')
        self.assertEqual((kept['how'], kept['actor'], kept['to_url']), ('hold', '', self.url), kept)
        self.assertEqual([(e.get('how'), e['action'], e['actor']) for e in self.record(host)],
                         [(None, 'click', ''), ('hold', 'submit', ''), ('hold', 'keep', '')])
        host.settle(1.0)
        self.assertEqual(host.latest_title(), 'Application')

    def test_what_a_page_sends_on_its_own_and_nobody_answers_is_set_down_as_dropped(self):
        # No action reached this request: the page sent it on its own, and
        # the hold is nobody's. The app drops it at its own deadline and
        # says that nobody answered.
        host = self.open('/auto')
        held = self.heard(host, lambda e: e['event'] == 'held')
        self.assertEqual((held['kind'], held['method']), ('request', 'POST'), held)
        began = self.set_down(host, 'request')
        self.assertEqual((began['seq'], began['actor'], began['url'], began['to_url'], began['message']),
                         (1, '', self.url + '/auto', self.url, 'POST ' + self.url), began)
        self.answer(host, held, False, reason='expired')
        dropped = self.set_down(host, 'drop')
        self.assertEqual((dropped['seq'], dropped['how'], dropped['actor'], dropped['message']),
                         (2, 'hold', '', 'POST ' + self.url), dropped)
        self.assertEqual([e['action'] for e in self.record(host)], ['request', 'drop'])
        # The body the page was about to send is in no part of the record.
        told = self.told(host)
        for text in ('4000056655665556', 'card='):
            self.assertNotIn(text, told)

    def test_a_hold_no_app_answers_ends_at_the_engines_own_deadline(self):
        # Nothing a page was about to send stays paused for good. A hold no
        # app answers in the time it may wait is dropped by the engine
        # itself, as one nobody answered, and the page's request ends unsent.
        host = self.open('/auto', wait_ms=1500)
        held = self.heard(host, lambda e: e['event'] == 'held')
        self.heard(host, lambda e: e['event'] == 'held_closed' and e['id'] == held['id'])
        dropped = self.set_down(host, 'drop')
        self.assertEqual((dropped['how'], dropped['actor'], dropped['message']),
                         ('hold', '', 'POST ' + self.url), dropped)
        self.shows(host, 'not sent')
        # An answer that comes after it answers nothing: nothing leaves.
        host.send(op='hold_answer', tab=held['tab'], id=held['id'], allow=True)
        host.settle(1.0)
        self.assertEqual([e['action'] for e in self.record(host)], ['request', 'drop'])
        self.shows(host, 'not sent')

    def test_a_hold_the_app_turns_away_as_one_too_many_is_set_down_as_such(self):
        # The app asks the person about so many holds at once and no more.
        # One beyond them it turns away at once and says why, and the record
        # says so: nobody was asked, and nothing left.
        host = self.open('/auto')
        held = self.heard(host, lambda e: e['event'] == 'held')
        self.answer(host, held, False, reason='full')
        turned = self.set_down(host, 'too_many')
        self.assertEqual((turned['how'], turned['actor'], turned['to_url'], turned['message']),
                         ('hold', '', self.url, 'POST ' + self.url), turned)
        self.assertEqual([e['action'] for e in self.record(host)], ['request', 'too_many'])
        self.shows(host, 'not sent')

    def test_a_held_download_no_app_answers_ends_at_the_engines_own_deadline(self):
        host = self.open('/download.html', wait_ms=1500)
        self.click(host, 'link "Receipt"')
        prompt = self.heard(host, lambda e: e['event'] == 'download_prompt')
        dropped = self.set_down(host, 'drop')
        self.assertEqual((dropped['actor'], dropped['message']), ('', 'receipt.txt, ' + self.url), dropped)
        self.heard(host, lambda e: e['event'] == 'download' and e['id'] == prompt['id']
                   and e['state'] == 'cancelled')

    def test_a_hold_whose_page_went_away_ends_and_is_set_down_as_gone(self):
        # The tab shows another page. What the page before it was held from
        # sending has no page to leave from: the hold ends, the app is told,
        # and the record says that the page went away, naming that page.
        host = self.open('/auto')
        held = self.heard(host, lambda e: e['event'] == 'held')
        host.navigate(self.url + '/')
        self.heard(host, lambda e: e['event'] == 'held_closed' and e['id'] == held['id'])
        gone = self.set_down(host, 'gone')
        self.assertEqual((gone['how'], gone['actor'], gone['url'], gone['to_url'], gone['message']),
                         ('hold', '', self.url + '/auto', self.url, 'POST ' + self.url), gone)
        self.assertEqual([e['action'] for e in self.record(host)], ['request', 'gone'])

    def test_a_held_submission_does_not_leave_from_another_page(self):
        # A held submission is issued again when the person allows it. Once
        # the tab shows another page, an allow that comes late sends nothing:
        # the form does not leave from a page the person never saw it on.
        host = self.open('/')
        self.click(host, 'button "Submit application"')
        held = self.heard(host, lambda e: e['event'] == 'held')
        host.navigate(self.url + '/script')
        self.heard(host, lambda e: e['event'] == 'held_closed' and e['id'] == held['id'])
        self.set_down(host, 'gone')
        host.send(op='hold_answer', tab=held['tab'], id=held['id'], allow=True)
        host.settle(1.5)
        self.assertEqual(host.latest_title(), 'Script departures')
        self.assertNotIn('Application received', [e.get('title') for e in host.seen])
        self.assertEqual([e['action'] for e in self.record(host)], ['click', 'submit', 'gone'])

    def test_a_held_download_is_set_down_with_its_file_and_who_answered(self):
        host = self.open('/download.html')
        self.click(host, 'link "Receipt"')
        prompt = self.heard(host, lambda e: e['event'] == 'download_prompt')
        began = self.set_down(host, 'download')
        self.assertEqual((began['how'], began['actor'], began['to_url'], began['message']),
                         ('hold', '', self.url, 'receipt.txt, ' + self.url), began)
        host.send(op='download_decision', tab=1, id=prompt['id'], allow=False, actor='rev')
        kept = self.set_down(host, 'keep')
        self.assertEqual((kept['actor'], kept['to_url'], kept['message']),
                         ('rev', self.url, 'receipt.txt, ' + self.url), kept)
        self.assertEqual([(e.get('how'), e['action']) for e in self.record(host)],
                         [(None, 'click'), ('hold', 'download'), ('hold', 'keep')])

    def test_a_window_nobody_told_to_hold_sends_what_is_clicked(self):
        host = self.open('/', hold=False)
        self.click(host, 'button "Submit application"')
        host.until(lambda e: e['event'] == 'title' and e['title'] == 'Application received')
        self.assertFalse([e for e in host.seen if e['event'] == 'held'])


if __name__ == '__main__':
    unittest.main()
