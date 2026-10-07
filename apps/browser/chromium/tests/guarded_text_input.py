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


FRAME = b'''<!doctype html><title>Frame field</title>
<input id="first" autofocus value="alpha beta gamma"><input id="second" value="second">
<textarea id="notes">alpha beta gamma</textarea>
<div id="rich" contenteditable>alpha <b>beta</b> gamma</div>
<script>
addEventListener('message', e=>{
 if(e.source!==parent || !e.data || !['inspect','focus'].includes(e.data.op)) return;
 if(e.data.op==='focus'){first.focus();first.setSelectionRange(6,10);}
 parent.postMessage({id:e.data.id,value:first.value,html:rich.innerHTML,focused:document.hasFocus(),active:document.activeElement?.id},e.origin);
});
</script>'''

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = FRAME if self.path.split('?')[0] == '/frame' else PAGE
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

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
        deadline = time.monotonic() + 10
        while not self.main_world("(first.focus(),first.setSelectionRange(6,10),document.hasFocus())"):
            self.assertLess(time.monotonic(), deadline, 'owned native document must acquire focus')
            time.sleep(.02)
        self.assertTrue(self.main_world("(first.focus(),first.setSelectionRange(6,10),document.hasFocus())"))

    def main_world(self, expression):
        nonce = time.monotonic_ns()
        # Document-start scripts apply to every frame; fixture control belongs
        # only to the top document, never to the field's descendant context.
        self.h.send(op='scripts', scripts=['if(window===window.top){document.title=JSON.stringify({value:(' + expression +
            '),nonce:' + str(nonce) + '})}'])
        value = json.loads(self.h.until(lambda e: e['event'] == 'title' and e['title'].startswith('{') and
            json.loads(e['title']).get('nonce') == nonce)['title'])['value']
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

    def frame_field(self):
        self.main_world("(()=>{const f=document.createElement('iframe');f.id='editorFrame';f.src='/frame';document.body.append(f);return true})()")
        deadline = time.monotonic() + 10
        while not self.main_world("!!editorFrame.contentDocument?.getElementById('first')"):
            self.assertLess(time.monotonic(), deadline, 'owned frame must load')
        self.assertTrue(self.main_world("(()=>{const f=editorFrame.contentDocument.getElementById('first');f.focus();f.setSelectionRange(6,10);return editorFrame.contentDocument.hasFocus()})()"))

    def test_focused_iframe_original_ack_unicode_and_native_undo(self):
        self.frame_field()
        lease = self.prepare()
        self.assertEqual(self.request_input('start', lease=lease)['data'], {'lease': lease})
        self.assertTrue(self.request_input('commit', lease=lease, text='日本語 café العربية')['ok'])
        self.assertEqual(self.main_world("editorFrame.contentDocument.getElementById('first').value"), 'alpha 日本語 café العربية gamma')
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        self.assertTrue(self.main_world("editorFrame.contentDocument.execCommand('undo')"))
        self.assertEqual(self.main_world("editorFrame.contentDocument.getElementById('first').value"), 'alpha beta gamma')

    def test_unrelated_sibling_updates_preserve_original_iframe_and_main_field(self):
        for frame in [False, True]:
            if frame:
                self.frame_field()
            lease = self.prepare()
            self.assertTrue(self.request_input('start', lease=lease)['ok'])
            self.main_world("(()=>{let n=document.createElement('aside');n.textContent='notification';document.body.append(n);n.remove();return true})()")
            self.assertEqual(self.request_input('check', lease=lease)['data'], {'lease': lease})
            self.assertTrue(self.request_input('commit', lease=lease, text='original field')['ok'])
            if frame:
                self.assertEqual(self.main_world("editorFrame.contentDocument.getElementById('first').value"), 'alpha original field gamma')
                self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
            else:
                self.assertEqual(self.main_world('first.value'), 'alpha original field gamma')
                self.assertTrue(self.main_world("document.execCommand('undo')"))

    def test_cross_origin_frame_remains_closed_without_observable_ancestors(self):
        # A distinct port is cross-origin while keeping both fixture documents
        # on one site; no site-isolation or Chromium sandbox flag is relaxed.
        other = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=other.serve_forever, daemon=True).start()
        self.addCleanup(other.server_close)
        self.addCleanup(other.shutdown)
        origin = f'http://127.0.0.1:{other.server_port}'
        self.main_world("(()=>{window.frameReply=null;addEventListener('message',e=>{if(e.origin===" + json.dumps(origin) + " && e.data.id==='owned-frame')window.frameReply=e.data;});const f=document.createElement('iframe');f.id='editorFrame';f.src=" + json.dumps(origin + '/frame') + ";f.onload=()=>{f.contentWindow.focus();f.contentWindow.postMessage({op:'focus',id:'owned-frame'}," + json.dumps(origin) + ");};document.body.append(f);return true})()")
        deadline = time.monotonic() + 10
        while self.main_world('frameReply') is None:
            self.assertLess(time.monotonic(), deadline, 'owned cross-origin field must be observed')
        # Cross-origin programmatic focus can be refused without activation.
        # Deliver real host pointer input into this owned fixture, then inspect
        # the child through its origin-checked message handshake.
        point = self.main_world("(()=>{const r=editorFrame.getBoundingClientRect();return {x:r.left+20,y:r.top+20}})()")
        self.h.send(op='mouse_button', button='left', down=True, **point)
        self.h.send(op='mouse_button', button='left', down=False, **point)
        self.main_world("(window.frameReply=null,editorFrame.contentWindow.postMessage({op:'focus',id:'owned-frame'}," + json.dumps(origin) + "),true)")
        while not self.main_world('frameReply?.focused===true'):
            self.assertLess(time.monotonic(), deadline, 'real pointer input must focus the owned cross-origin field: '+repr({'point':point,'reply':self.main_world('frameReply'),'active':self.main_world('document.activeElement?.id')}))
            self.main_world("(editorFrame.contentWindow.postMessage({op:'inspect',id:'owned-frame'}," + json.dumps(origin) + "),true)")
            time.sleep(0.01)
        self.assertTrue(self.main_world('frameReply.focused'))
        self.assertTrue(self.main_world('editorFrame.contentDocument===null'))
        result = self.request_input('prepare')
        self.assertFalse(result['ok'])
        self.assertEqual(result['code'], 'unavailable')
        self.assertEqual(self.main_world('frameReply.value'), 'alpha beta gamma')
        self.unchanged()

    def test_iframe_replacement_refuses_old_final_and_allows_new_original_session(self):
        self.frame_field()
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.main_world("(editorFrame.remove(),true)")
        result = self.request_input('commit', lease=lease, text='must not retarget')
        self.assertFalse(result['ok'])
        self.assertEqual(result['code'], 'text_input_retired')
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        self.frame_field()
        fresh = self.prepare()
        self.assertNotEqual(fresh, lease)
        self.assertFalse(self.request_input('cancel', lease=lease)['ok'])
        self.assertTrue(self.request_input('start', lease=fresh)['ok'])
        self.assertTrue(self.request_input('commit', lease=fresh, text='new frame')['ok'])
        self.assertEqual(self.main_world("editorFrame.contentDocument.getElementById('first').value"), 'alpha new frame gamma')

    def test_iframe_readonly_ancestor_revocation_is_not_cleared_by_restore(self):
        self.frame_field()
        self.main_world("(editorFrame.setAttribute('aria-readonly','true'),true)")
        self.assertFalse(self.request_input('prepare')['ok'])
        self.main_world("(editorFrame.removeAttribute('aria-readonly'),true)")
        lease = self.prepare()
        self.main_world("(editorFrame.setAttribute('aria-readonly','true'),editorFrame.removeAttribute('aria-readonly'),true)")
        self.assertFalse(self.request_input('commit', lease=lease, text='must refuse')['ok'])
        self.assertEqual(self.main_world("editorFrame.contentDocument.getElementById('first').value"), 'alpha beta gamma')

    def test_bounded_session_start_outlives_warm_expiry_without_sliding(self):
        # Real monotonic time against the production timer, without clock hooks.
        def wait_until(deadline):
            while time.monotonic() < deadline:
                time.sleep(min(1, deadline - time.monotonic()))

        lease = self.prepare()
        prepared = time.monotonic()
        print('LEASE lifetime: original warm lease prepared', flush=True)
        wait_until(prepared + 20)
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        started = time.monotonic()
        print('LEASE lifetime: explicit original session acknowledged', flush=True)
        wait_until(prepared + 181)
        self.assertEqual(self.request_input('check', lease=lease)['data'], {'lease': lease})
        self.assertFalse(self.request_input('start', lease=lease)['ok'])
        print('LEASE lifetime: original survives warm expiry; replay cannot extend it', flush=True)
        wait_until(started + 181)
        result = self.request_input('check', lease=lease)
        self.assertFalse(result['ok'])
        self.assertEqual(result['code'], 'text_input_retired')
        self.unchanged()
        print('LEASE lifetime: fixed active-session bound expired without editing', flush=True)

    def test_private_text_operations_never_create_site_user_activation(self):
        self.assertFalse(self.main_world('navigator.userActivation.isActive'))
        lease = self.prepare()
        self.assertFalse(self.main_world('navigator.userActivation.isActive'))
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertFalse(self.main_world('navigator.userActivation.isActive'))
        self.assertTrue(self.request_input('check', lease=lease)['ok'])
        self.assertFalse(self.main_world('navigator.userActivation.isActive'))
        self.assertTrue(self.request_input('commit', lease=lease, text='private input')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha private input gamma')
        self.assertFalse(self.main_world('navigator.userActivation.isActive'))

    def test_explicit_start_is_one_shot_and_preserves_the_original_selection(self):
        lease = self.prepare()
        self.assertEqual(self.request_input('start', lease=lease)['data'], {'lease': lease})
        self.assertFalse(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('check', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='consented')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha consented gamma')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.unchanged()

    def test_start_cannot_acquire_a_changed_or_expired_original_target(self):
        lease = self.prepare()
        self.assertFalse(self.request_input('start', lease=lease, expires_ms=1)['ok'])
        self.assertTrue(self.request_input('check', lease=lease)['ok'])
        self.main_world('(second.focus(),first.focus(),first.setSelectionRange(6,10),true)')
        self.assertFalse(self.request_input('start', lease=lease)['ok'])
        self.unchanged()

    def test_private_lease_check_observes_the_original_edit_stamp(self):
        lease = self.prepare()
        self.assertEqual(self.request_input('check', lease=lease)['data'], {'lease': lease})
        self.main_world("(first.setSelectionRange(0,5),true)")
        self.assertFalse(self.request_input('check', lease=lease)['ok'])
        self.assertFalse(self.request_input('commit', lease=lease, text='stale')['ok'])
        self.unchanged()

    def test_expired_delivery_cannot_insert_or_consume_a_successor(self):
        lease = self.prepare()
        result = self.request_input('commit', lease=lease, text='expired', expires_ms=1)
        self.assertFalse(result['ok'])
        self.unchanged()
        self.assertTrue(self.request_input('check', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='fresh')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha fresh gamma')

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

    def test_original_field_history_flags_follow_native_undo_and_redo(self):
        result = self.request_input('prepare')
        self.assertTrue(result['ok'], result)
        self.assertIn('history', result, 'original-field ACK must observe native history')
        self.assertEqual(result['history'], {'can_undo': False, 'can_redo': False})
        lease = result['data']['lease']
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='dictated')['ok'])
        self.main_world('(document.queryCommandEnabled=()=>false,true)')
        self.main_world('(document.execCommand=()=>false,true)')
        for direction, value, flags in [
                ('undo', 'alpha beta gamma', {'can_undo': True, 'can_redo': False}),
                ('redo', 'alpha dictated gamma', {'can_undo': False, 'can_redo': True})]:
            result = self.request_input('prepare')
            self.assertTrue(result['ok'], result)
            self.assertEqual(result['history'], flags)
            self.assertEqual(set(result['data']), {'lease'}, 'history exports no field content')
            lease = result['data']['lease']
            check = self.request_input('check', lease=lease)
            self.assertTrue(check['ok'], check)
            self.assertEqual(check['history'], flags)
            effect = self.request_input(direction, lease=lease, history_scope='page')
            self.assertTrue(effect['ok'], effect)
            self.assertEqual(self.main_world('first.value'), value)
            self.assertFalse(self.request_input(direction, lease=lease)['ok'], 'history is one shot')
        lease = self.prepare()
        self.main_world('(second.focus(),true)')
        rejected = self.request_input('undo', lease=lease, history_scope='page')
        self.assertFalse(rejected['ok'], rejected)
        self.assertNotIn('history', rejected, 'changed field must not report original history')
        self.assertEqual(self.main_world('first.value'), 'alpha dictated gamma')
        self.assertEqual(self.main_world('second.value'), 'second')

    def test_original_field_history_never_undoes_a_successor_entry(self):
        lease = self.prepare()
        self.assertTrue(self.request_input('commit', lease=lease, text='original')['ok'])
        self.assertTrue(self.main_world("(second.focus(),second.select(),document.execCommand('insertText',false,'successor edit'))"))
        self.main_world('(first.focus(),first.setSelectionRange(6,14),true)')
        lease = self.prepare()
        result = self.request_input('undo', lease=lease)
        self.assertEqual(self.main_world('second.value'), 'successor edit', 'original-field Undo cannot mutate a different field history entry')
        if result['ok']:
            self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        else:
            self.assertEqual(self.main_world('first.value'), 'alpha original gamma')

    def test_explicit_page_history_matches_native_cross_field_stack(self):
        lease = self.prepare()
        self.assertTrue(self.request_input('commit', lease=lease, text='original')['ok'])
        self.assertTrue(self.main_world("(second.focus(),second.select(),document.execCommand('insertText',false,'successor edit'))"))
        self.main_world('(first.focus(),first.setSelectionRange(6,14),true)')
        lease = self.prepare()
        result = self.request_input('undo', lease=lease, history_scope='page')
        self.assertTrue(result['ok'], result)
        self.assertEqual(self.main_world('first.value'), 'alpha original gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertFalse(self.request_input('undo', lease=lease, history_scope='page')['ok'])

    def test_selectionless_dom_snapshot_cannot_bind_two_native_carets(self):
        for kind, value, insertion in [('email', 'public@example.test', 'safe'),
                                       ('number', '123.5', '7')]:
            with self.subTest(kind=kind):
                self.main_world("(()=>{const e=document.createElement('input');e.id='limitedField';"
                    f"e.type={json.dumps(kind)};e.value={json.dumps(value)};"
                    "document.body.append(e);e.focus();window.selectionProbeKeys=0;"
                    "window.selectionProbeListener=e=>{if(['ArrowLeft','ArrowRight'].includes(e.key))"
                    "window.selectionProbeKeys++};document.addEventListener('keydown',selectionProbeListener);return true})()")
                try:
                    observations, edits = [], []
                    for move_right in [False, True]:
                        self.main_world('(limitedField.select(),true)')
                        keys = ['ArrowLeft', 'ArrowRight'] if move_right else ['ArrowLeft']
                        expected = self.main_world('selectionProbeKeys') + len(keys)
                        for key in keys:
                            self.h.send(op='key', key=key, code=key, down=True)
                            self.h.send(op='key', key=key, code=key, down=False)
                        deadline = time.monotonic() + 5
                        while self.main_world('selectionProbeKeys') < expected:
                            self.assertLess(time.monotonic(), deadline, 'native key must reach owned renderer')
                            time.sleep(.01)
                        observations.append(self.main_world("(()=>{const s=getSelection();return {"
                            "value:limitedField.value,start:limitedField.selectionStart,"
                            "end:limitedField.selectionEnd,anchor:s.anchorNode.nodeName,"
                            "focus:s.focusNode.nodeName,anchorOffset:s.anchorOffset,"
                            "focusOffset:s.focusOffset,text:s.toString(),type:s.type}})()"))
                        self.assertEqual(observations[-1]['type'], 'Caret')
                        refusal = self.request_input('prepare')
                        self.assertFalse(refusal['ok'], refusal)
                        self.assertEqual(refusal['code'], 'not_editable', refusal)
                        self.assertTrue(self.main_world('document.execCommand("insertText",false,' +
                                                       json.dumps(insertion) + ')'))
                        edits.append(self.main_world('limitedField.value'))
                        self.assertTrue(self.main_world('document.execCommand("undo")'))
                        self.assertEqual(self.main_world('limitedField.value'), value)
                    self.assertEqual(observations[0], observations[1],
                        'DOM field/document selection must not pretend to identify a hidden native caret')
                    self.assertEqual(edits, [insertion + value, value[0] + insertion + value[1:]])
                    self.unchanged()
                finally:
                    self.main_world("(document.removeEventListener('keydown',selectionProbeListener),limitedField.remove(),true)")

    def test_selectionless_email_and_number_refuse_deferred_input_and_recover(self):
        for kind, value in [('email', 'public@example.test'), ('number', '123.5')]:
            with self.subTest(kind=kind):
                snapshot = self.main_world("(()=>{let e=document.createElement('input');"
                    f"e.id='limitedField';e.type={json.dumps(kind)};e.value={json.dumps(value)};"
                    "document.body.append(e);e.focus();return {focused:document.activeElement===e,"
                    "start:e.selectionStart,end:e.selectionEnd,value:e.value}})()")
                self.assertEqual(snapshot, {'focused': True, 'start': None, 'end': None, 'value': value})
                result = self.request_input('prepare')
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['code'], 'not_editable', result)
                self.assertEqual(self.main_world('limitedField.value'), value)
                self.unchanged()
                self.main_world('(limitedField.remove(),true)')
        # Rejection must not leave a lease that blocks a supported successor.
        self.main_world('(first.focus(),first.setSelectionRange(6,10),true)')
        lease = self.prepare()
        self.assertEqual(self.request_input('start', lease=lease)['data'], {'lease': lease})
        self.assertTrue(self.request_input('commit', lease=lease, text='recovered')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha recovered gamma')
        self.assertEqual(self.main_world('second.value'), 'second')

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

    def test_transient_css_masking_permanently_retires_the_lease(self):
        lease = self.prepare()
        self.main_world("(first.style.webkitTextSecurity='disc',first.style.webkitTextSecurity='none',true)")
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.unchanged()

    def test_shadow_host_readonly_is_part_of_field_eligibility(self):
        for attribute in ['aria-readonly', 'aria-disabled', 'inert']:
            with self.subTest(attribute=attribute):
                self.main_world(f"(shadow.shadowRoot.querySelector('input').focus(),shadow.setAttribute('{attribute}','true'),true)")
                self.assertFalse(self.request_input('prepare')['ok'])
                self.assertEqual(self.main_world("shadow.shadowRoot.querySelector('input').value"), 'alpha beta gamma')
                self.main_world(f"(shadow.removeAttribute('{attribute}'),true)")

    def test_nested_shadow_host_revocation_and_restore_retire_the_original(self):
        self.main_world("(()=>{shadow.shadowRoot.innerHTML='<div id=inner></div>';const inner=shadow.shadowRoot.querySelector('#inner');inner.attachShadow({mode:'open'}).innerHTML='<input value=original>';const field=inner.shadowRoot.querySelector('input');field.focus();field.setSelectionRange(0,8);return true})()")
        lease = self.prepare()
        self.main_world("(()=>{const inner=shadow.shadowRoot.querySelector('#inner');inner.setAttribute('aria-readonly','true');inner.removeAttribute('aria-readonly');return true})()")
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.assertEqual(self.main_world("shadow.shadowRoot.querySelector('#inner').shadowRoot.querySelector('input').value"), 'original')
        self.assertEqual(self.main_world('second.value'), 'second')

    def test_readonly_revocation_and_restore_still_retire_the_lease(self):
        lease = self.prepare()
        self.main_world('(first.readOnly=true,first.readOnly=false,true)')
        self.assertFalse(self.request_input('commit', lease=lease, text='lost')['ok'])
        self.unchanged()


if __name__ == '__main__':
    unittest.main()
