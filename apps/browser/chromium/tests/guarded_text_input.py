#!/usr/bin/env python3
"""Private guarded insertion through the actual CEF host, without speech/mic."""
import http.server
import json
import threading
import time
import unittest
from native_regressions import Host

PAGE = b'''<!doctype html><title>Guarded input</title>
<input id="first" autofocus value="alpha beta gamma"><input id="second" value="second">
<textarea id="notes">alpha beta gamma</textarea>
<input id="password" type="password" value="private"><input id="otp" autocomplete="one-time-code">
<input id="card" autocomplete="CC-number"><input id="masked" style="-webkit-text-security:disc">
<div id="rich" contenteditable>alpha <b>beta</b> gamma</div><div id="shadow"></div>
<script>
shadow.attachShadow({mode:'open'}).innerHTML='<input value="alpha beta gamma">';
window.editEvents=[];
addEventListener('input', e=>editEvents.push(e.inputType), true);
addEventListener('beforeinput', e=>{
 if(window.rejectEdit) e.preventDefault();
 if(window.redirectEdit) second.focus();
}, true);
</script>'''


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)

    def log_message(self, *_):
        pass


class GuardedTextInput(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f'http://127.0.0.1:{cls.server.server_port}/input'

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.h = Host()
        self.addCleanup(self.h.close)
        self.h.send(op='visibility', visible=True)
        self.h.send(op='resize', width=1000, height=800, scale_factor=1.0)
        self.h.navigate(self.url)
        self.h.send(op='focus', focused=True)
        self.request = 0
        self.assertTrue(self.main_world("(first.focus(),first.setSelectionRange(6,10),document.hasFocus())"))

    def main_world(self, expression):
        self.h.send(op='scripts', scripts=['document.title=JSON.stringify({value:(' + expression +
            '),nonce:' + str(time.monotonic_ns()) + '})'])
        value = json.loads(self.h.until(lambda e: e['event'] == 'title' and e['title'].startswith('{'))['title'])['value']
        self.h.send(op='scripts', scripts=[])
        return value

    def request_input(self, action, **fields):
        self.request += 1
        request = self.request
        self.h.send(op='text_input', request_id=request, action=action + '_text_input', **fields)
        return self.h.until(lambda e: e['event'] == 'text_input_result' and e['request_id'] == request)

    def prepare(self):
        result = self.request_input('prepare')
        self.assertTrue(result['ok'], result)
        self.assertEqual(set(result['data']), {'lease'}, 'preparation must export no field content')
        return result['data']['lease']

    def unchanged(self, target='first'):
        self.assertEqual(self.main_world(f'{target}.value'), 'alpha beta gamma')
        self.assertEqual(self.main_world('second.value'), 'second')

    def test_selected_unicode_replacement_is_one_native_undo_and_cannot_repeat(self):
        lease = self.prepare()
        result = self.request_input('commit', lease=lease, text='日本語 café العربية')
        self.assertTrue(result['ok'], result)
        self.assertEqual(self.main_world('first.value'), 'alpha 日本語 café العربية gamma')
        self.assertIn('insertText', self.main_world('editEvents'))
        self.assertFalse(self.request_input('commit', lease=lease, text='again')['ok'])
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.unchanged()
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('first.value'), 'alpha 日本語 café العربية gamma')

    def test_focus_change_and_return_permanently_retires_original_lease(self):
        lease = self.prepare()
        self.main_world('(second.focus(),first.focus(),first.setSelectionRange(6,10),true)')
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.unchanged()

    def test_value_selection_and_readonly_changes_prevent_insertion(self):
        for change in ["first.value='different'", 'first.setSelectionRange(0,0)',
                       'first.readOnly=true', "first.type='password'"]:
            with self.subTest(change=change):
                self.main_world("(first.type='text',first.readOnly=false,first.value='alpha beta gamma',first.focus(),first.setSelectionRange(6,10),true)")
                lease = self.prepare()
                self.main_world('(' + change + ',true)')
                self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
                self.assertNotIn('lost', self.main_world('first.value'))
                self.assertEqual(self.main_world('second.value'), 'second')

    def test_cancel_and_retired_cancel_cannot_affect_a_successor(self):
        old = self.prepare()
        self.assertTrue(self.request_input('cancel', lease=old)['ok'])
        self.assertFalse(self.request_input('commit', lease=old, text='lost')['ok'])
        current = self.prepare()
        self.assertNotEqual(old, current)
        self.assertFalse(self.request_input('cancel', lease=old)['ok'])
        self.assertTrue(self.request_input('commit', lease=current, text='new')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha new gamma')

    def test_beforeinput_denial_and_focus_redirect_do_not_fallback(self):
        for flag in ['rejectEdit', 'redirectEdit']:
            with self.subTest(flag=flag):
                self.main_world('(first.focus(),first.setSelectionRange(6,10),true)')
                lease = self.prepare()
                self.main_world('(window.' + flag + '=true,true)')
                self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
                self.unchanged()
                self.main_world('(window.' + flag + '=false,true)')

    def test_password_otp_and_composition_are_refused(self):
        for target in ['password', 'otp', 'card', 'masked']:
            self.main_world(f'({target}.focus(),true)')
            result = self.request_input('prepare')
            self.assertFalse(result['ok'], result)
            self.assertEqual(result['code'], 'secret_field')
        self.main_world('(first.focus(),first.setSelectionRange(6,10),true)')
        lease = self.prepare()
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true})),true)")
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.assertFalse(self.request_input('prepare')['ok'])
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),true)")
        self.assertTrue(self.request_input('prepare')['ok'])
        self.unchanged()

    def test_navigation_invalidates_the_original_document_lease(self):
        lease = self.prepare()
        self.h.navigate(self.url + '?next')
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.unchanged()

    def test_textarea_and_open_shadow_input_keep_the_original_selection(self):
        for target in ['notes', "shadow.shadowRoot.querySelector('input')"]:
            with self.subTest(target=target):
                self.main_world(f'({target}.focus(),{target}.setSelectionRange(6,10),true)')
                lease = self.prepare()
                self.assertTrue(self.request_input('commit', lease=lease, text='new')['ok'])
                self.assertEqual(self.main_world(f'{target}.value'), 'alpha new gamma')
        self.unchanged()

    def test_rich_selection_preserves_structure_and_native_undo(self):
        self.main_world("(()=>{rich.focus();const range=document.createRange();range.selectNodeContents(rich.querySelector('b'));const s=getSelection();s.removeAllRanges();s.addRange(range);return true})()")
        lease = self.prepare()
        self.assertTrue(self.request_input('commit', lease=lease, text='new')['ok'])
        self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <b>new</b> gamma')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <b>beta</b> gamma')

    def test_private_bridge_is_inaccessible_to_page_script(self):
        lease = self.prepare()
        self.assertEqual(self.main_world('typeof globalThis.__tiloPage'), 'undefined')
        self.assertNotIn(lease, json.dumps(self.main_world('Object.keys(globalThis)')))

    def test_window_focus_loss_prevents_insertion_after_focus_returns(self):
        lease = self.prepare()
        self.h.send(op='focus', focused=False)
        self.assertFalse(self.main_world('document.hasFocus()'))
        self.h.send(op='focus', focused=True)
        self.assertTrue(self.main_world('document.hasFocus()'))
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.unchanged()

    def test_hidden_view_cannot_prepare_or_commit_but_can_cancel(self):
        lease = self.prepare()
        self.h.send(op='visibility', visible=False)
        self.assertEqual(self.request_input('prepare')['code'], 'unavailable')
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.request_input('cancel', lease=lease)
        self.unchanged()

    def test_readonly_revocation_and_restore_still_retire_the_lease(self):
        lease = self.prepare()
        self.main_world('(first.readOnly=true,first.readOnly=false,true)')
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.unchanged()


if __name__ == '__main__':
    unittest.main()
