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
window.editEvents=[];window.beforeEdits=0;
addEventListener('input', e=>editEvents.push(e.inputType), true);
addEventListener('beforeinput', e=>{
 window.beforeEdits++;
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
        self.assertEqual(set(result['data']), {'lease', 'multiline'}, 'preparation must export no field content')
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

    def native_field(self, kind, value):
        self.main_world("(()=>{document.getElementById('nativeField')?.remove();const e=document.createElement('input');e.id='nativeField';e.type=" + json.dumps(kind) + ";e.value=" + json.dumps(value) + ";document.body.append(e);e.focus();e.select();return true})()")

    def native_prepare(self):
        ready = next(event for event in self.h.seen if event['event'] == 'ready')
        self.assertIs(ready.get('native_caret_text_input'), True)
        result = self.request_input('prepare', native_caret=True)
        self.assertTrue(result['ok'], result)
        self.assertEqual(set(result['data']), {'lease', 'multiline'})
        self.assertIs(result['data']['multiline'], False)
        return result['data']['lease']

    def test_multiline_paragraph_replacement_is_one_native_history_edit(self):
        paragraph = 'First café.\n\nSecond 👋'
        for target in ['notes', 'rich']:
            with self.subTest(target=target):
                self.main_world("(()=>{const e=" + target + ";e.focus();if(e.setSelectionRange)e.setSelectionRange(6,10);else{const r=document.createRange();r.selectNodeContents(e.querySelector('b'));getSelection().removeAllRanges();getSelection().addRange(r)}return true})()")
                result = self.request_input('prepare')
                self.assertTrue(result['ok'], result)
                self.assertIs(result['data']['multiline'], True)
                lease = result['data']['lease']
                self.assertEqual(self.request_input('start', lease=lease)['data'],
                                 {'lease': lease, 'multiline': True})
                self.main_world('(editEvents=[],true)')
                self.assertTrue(self.request_input('commit', lease=lease, text=paragraph)['ok'])
                self.assertEqual(self.main_world('editEvents'),
                                 ['insertText', 'insertLineBreak', 'insertLineBreak', 'insertText'])
                expression = 'notes.value' if target == 'notes' else 'rich.innerText'
                self.assertEqual(self.main_world(expression), 'alpha ' + paragraph + ' gamma')
                if target == 'rich':
                    self.assertEqual(self.main_world('rich.innerHTML'),
                                     'alpha <b>First café.<br><br>Second 👋</b> gamma')
                self.assertTrue(self.main_world("document.execCommand('undo')"))
                self.assertEqual(self.main_world(expression), 'alpha beta gamma')
                self.assertTrue(self.main_world("document.execCommand('redo')"))
                self.assertEqual(self.main_world(expression), 'alpha ' + paragraph + ' gamma')
                self.assertFalse(self.request_input('commit', lease=lease, text=paragraph)['ok'])
                self.assertFalse(self.main_world('navigator.userActivation.isActive'))
                self.assertTrue(self.main_world("document.execCommand('undo')"))

    def test_paragraph_only_and_trailing_breaks_keep_selection_and_history(self):
        self.main_world('(rich.style.whiteSpace="pre-wrap",true)')
        for target in ['notes', 'rich']:
            for paragraph, events in [('\n\n', ['insertLineBreak', 'insertLineBreak']),
                                      ('First café.\n', ['insertText', 'insertLineBreak'])]:
                with self.subTest(target=target, paragraph=repr(paragraph)):
                    self.main_world("(()=>{const e=" + target + ";e.focus();if(e.setSelectionRange)e.setSelectionRange(6,10);else{const r=document.createRange();r.selectNodeContents(e.querySelector('b'));getSelection().removeAllRanges();getSelection().addRange(r)}editEvents=[];return true})()")
                    original = self.main_world(target + ('.value' if target == 'notes' else '.innerHTML'))
                    lease = self.prepare()
                    self.assertTrue(self.request_input('start', lease=lease)['ok'])
                    self.assertTrue(self.request_input('commit', lease=lease, text=paragraph)['ok'])
                    self.assertEqual(self.main_world('editEvents'), events)
                    expression = target + ('.value' if target == 'notes' else '.innerText')
                    self.assertEqual(self.main_world(expression), 'alpha ' + paragraph + ' gamma')
                    self.assertTrue(self.main_world("document.execCommand('undo')"))
                    self.assertEqual(self.main_world(target + ('.value' if target == 'notes' else '.innerHTML')), original)
                    self.assertTrue(self.main_world("document.execCommand('redo')"))
                    self.assertEqual(self.main_world(expression), 'alpha ' + paragraph + ' gamma')
                    self.assertTrue(self.main_world("document.execCommand('undo')"))

    def test_collapsing_rich_whitespace_rejects_boundary_edits_before_handlers(self):
        for text in ['\n\n', 'First\n', '\nFirst', 'First \nSecond', 'First\n Second']:
            with self.subTest(text=repr(text)):
                self.main_world("(()=>{rich.focus();const r=document.createRange();r.selectNodeContents(rich.querySelector('b'));getSelection().removeAllRanges();getSelection().addRange(r);return true})()")
                lease = self.prepare()
                self.assertTrue(self.request_input('start', lease=lease)['ok'])
                before = self.main_world('beforeEdits')
                result = self.request_input('commit', lease=lease, text=text)
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['code'], 'rejected')
                self.assertEqual(self.main_world('beforeEdits'), before)
                self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <b>beta</b> gamma')
                self.assertFalse(self.request_input('commit', lease=lease, text='retry')['ok'])

    def test_rich_paragraph_retains_native_inline_typing_format(self):
        self.main_world("(()=>{const e=rich.querySelector('b');e.style.cssText='color: rgb(123, 45, 67);font-style:italic;font-family:monospace';rich.focus();const r=document.createRange();r.selectNodeContents(e);getSelection().removeAllRanges();getSelection().addRange(r);return true})()")
        original = self.main_world('rich.innerHTML')
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='First\n\nSecond')['ok'])
        self.assertEqual(self.main_world('rich.innerText'), 'alpha First\n\nSecond gamma')
        self.assertEqual(self.main_world("(()=>{const w=document.createTreeWalker(rich,NodeFilter.SHOW_TEXT);while(w.nextNode()){if(w.currentNode.data.includes('Second')){const s=getComputedStyle(w.currentNode.parentElement);return [s.fontWeight,s.fontStyle,s.color,s.fontFamily]}}return null})()"),
                         ['700', 'italic', 'rgb(123, 45, 67)', 'monospace'])
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('rich.innerHTML'), original)
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('rich.innerText'), 'alpha First\n\nSecond gamma')

    def test_rich_paragraph_mixed_selection_keeps_native_start_format(self):
        self.main_world("(()=>{rich.focus();const r=document.createRange();r.setStart(rich.querySelector('b').firstChild,2);r.setEnd(rich.lastChild,2);getSelection().removeAllRanges();getSelection().addRange(r);return true})()")
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='First\n\nSecond')['ok'])
        self.assertEqual(self.main_world('rich.innerText'), 'alpha beFirst\n\nSecondamma')
        self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <b>beFirst<br><br>Second</b>amma')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <b>beta</b> gamma')
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <b>beFirst<br><br>Second</b>amma')

    def test_rich_paragraph_preserves_nested_editor_structure_and_history(self):
        cases = [
            ('<code>alpha <b>beta</b> gamma</code>', 'code'),
            ('<a href="/kept">alpha <b>beta</b> gamma</a>', 'a'),
            ('<ul><li>alpha <b>beta</b> gamma</li></ul>', 'li'),
            ('<table><tbody><tr><td>alpha <b>beta</b> gamma</td></tr></tbody></table>', 'td'),
        ]
        for markup, container in cases:
            with self.subTest(container=container):
                self.main_world("(()=>{rich.innerHTML=" + json.dumps(markup) + ";rich.focus();const r=document.createRange();r.selectNodeContents(rich.querySelector('b'));getSelection().removeAllRanges();getSelection().addRange(r);return true})()")
                lease = self.prepare()
                self.assertTrue(self.request_input('start', lease=lease)['ok'])
                self.assertTrue(self.request_input('commit', lease=lease, text='First\n\nSecond')['ok'])
                self.assertEqual(self.main_world("rich.querySelector(" + json.dumps(container) + ").innerText"), 'alpha First\n\nSecond gamma')
                self.assertEqual(self.main_world("rich.querySelector('b').innerHTML"), 'First<br><br>Second')
                if container == 'a':
                    self.assertEqual(self.main_world("rich.querySelector('a').getAttribute('href')"), '/kept')
                edited = self.main_world('rich.innerHTML')
                self.assertTrue(self.main_world("document.execCommand('undo')"))
                self.assertEqual(self.main_world('rich.innerHTML'), markup)
                self.assertTrue(self.main_world("document.execCommand('redo')"))
                self.assertEqual(self.main_world('rich.innerHTML'), edited)
                self.assertFalse(self.request_input('commit', lease=lease, text='repeat')['ok'])

    def test_rich_noneditable_islands_cannot_be_replaced_or_crossed(self):
        for crossing in [False, True]:
            with self.subTest(crossing=crossing):
                self.main_world("(()=>{rich.innerHTML='alpha <span contenteditable=false>locked</span> gamma';rich.focus();const r=document.createRange();" + ("r.setStart(rich.firstChild,2);r.setEnd(rich.lastChild,2);" if crossing else "r.selectNodeContents(rich.querySelector('span'));") + "getSelection().removeAllRanges();getSelection().addRange(r);beforeEdits=0;editEvents=[];return true})()")
                result = self.request_input('prepare')
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['code'], 'not_editable')
                self.assertEqual(self.main_world('rich.innerHTML'), 'alpha <span contenteditable="false">locked</span> gamma')
                self.assertEqual(self.main_world('beforeEdits'), 0)
                self.assertEqual(self.main_world('editEvents'), [])

    def test_rich_editable_text_beside_locked_island_still_accepts_dictation(self):
        self.main_world("(()=>{rich.innerHTML='alpha <span contenteditable=false>locked</span> gamma';rich.focus();const r=document.createRange();r.setStart(rich.lastChild,1);r.setEnd(rich.lastChild,6);getSelection().removeAllRanges();getSelection().addRange(r);return true})()")
        original = self.main_world('rich.innerHTML')
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='First\nSecond')['ok'])
        self.assertEqual(self.main_world('rich.innerText'), 'alpha locked First\nSecond')
        self.assertEqual(self.main_world("rich.querySelector('span').outerHTML"), '<span contenteditable="false">locked</span>')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('rich.innerHTML'), original)
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('rich.innerText'), 'alpha locked First\nSecond')

    def test_paragraph_undo_does_not_absorb_previous_native_typing(self):
        self.main_world("(notes.focus(),notes.setSelectionRange(notes.value.length,notes.value.length),document.execCommand('insertText',false,' typed'))")
        previous = self.main_world('notes.value')
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='First\n\nSecond')['ok'])
        self.assertEqual(self.main_world('notes.value'), previous + 'First\n\nSecond')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('notes.value'), previous)
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('notes.value'), previous + 'First\n\nSecond')

    def test_paragraph_input_handler_cannot_redirect_remaining_text(self):
        self.main_world('(notes.focus(),notes.setSelectionRange(6,10),true)')
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.main_world("(notes.addEventListener('input',()=>second.focus(),{once:true}),true)")
        result = self.request_input('commit', lease=lease, text='First\nSecond')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_changed')
        self.assertEqual(self.main_world('notes.value'), 'alpha First gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertFalse(self.request_input('commit', lease=lease, text='Second')['ok'])
        self.main_world('(notes.focus(),true)')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('notes.value'), 'alpha beta gamma')

    def test_final_capacity_cannot_truncate_or_delete_original_selection(self):
        for target, text in [('first', 'long replacement'), ('notes', 'First\n\nSecond')]:
            with self.subTest(target=target):
                self.main_world('('+target+'.focus(),'+target+'.setSelectionRange(6,10),'+target+'.maxLength=18,true)')
                lease = self.prepare()
                self.assertTrue(self.request_input('start', lease=lease)['ok'])
                before = self.main_world('beforeEdits')
                result = self.request_input('commit', lease=lease, text=text)
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['code'], 'rejected')
                self.assertEqual(self.main_world('beforeEdits'), before)
                self.assertEqual(self.main_world(target+'.value'), 'alpha beta gamma')
                self.main_world('('+target+'.removeAttribute("maxlength"),true)')

    def test_paragraph_handler_capacity_change_interrupts_remaining_edit(self):
        self.main_world('(notes.focus(),notes.setSelectionRange(6,10),true)')
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.main_world("(notes.addEventListener('input',()=>notes.maxLength=2,{once:true}),true)")
        result = self.request_input('commit', lease=lease, text='First\nSecond')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_changed')
        self.assertEqual(self.main_world('notes.value'), 'alpha First gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('notes.value'), 'alpha beta gamma')

    def test_control_characters_never_delete_the_original_selection(self):
        for target, values in [('first', ['a\nb', '\n\n']),
                               ('notes', []), ('rich', [])]:
            for text in values + ['a\rb', 'a\tb', 'a\x00b', 'a\x1bb', 'a\x85b']:
                with self.subTest(target=target, text=repr(text)):
                    self.main_world("(()=>{const e=" + target + ";e.focus();if(e.setSelectionRange)e.setSelectionRange(6,10);else{const r=document.createRange();r.selectNodeContents(e.querySelector('b'));getSelection().removeAllRanges();getSelection().addRange(r)}return true})()")
                    lease = self.prepare()
                    self.assertTrue(self.request_input('start', lease=lease)['ok'])
                    before = self.main_world('beforeEdits')
                    result = self.request_input('commit', lease=lease, text=text)
                    self.assertEqual(self.main_world('beforeEdits'), before)
                    self.assertFalse(result['ok'], result)
                    self.assertEqual(result['code'], 'rejected')
                    expression = target + ('.innerText' if target == 'rich' else '.value')
                    self.assertEqual(self.main_world(expression), 'alpha beta gamma')

    def test_single_line_kinds_reject_paragraphs_before_page_handlers(self):
        for kind, value in [('text', 'original'), ('search', 'original'),
                            ('email', 'public@example.test'), ('number', '123.5')]:
            for text in ['first\nsecond', '\n\n']:
                with self.subTest(kind=kind, text=repr(text)):
                    self.native_field(kind, value)
                    lease = self.native_prepare()
                    self.assertTrue(self.request_input('start', lease=lease)['ok'])
                    before = self.main_world('beforeEdits')
                    result = self.request_input('commit', lease=lease, text=text)
                    self.assertFalse(result['ok'], result)
                    self.assertEqual(result['code'], 'rejected')
                    self.assertEqual(self.main_world('nativeField.value'), value)
                    self.assertEqual(self.main_world('beforeEdits'), before)

    def test_native_email_number_replace_cancel_repeat_and_history(self):
        for kind, value, replacement in [('email', 'public@example.test', 'café@example.test'),
                                          ('number', '123.5', '42.75'), ('email', '', 'a@b.test'), ('number', '', '7')]:
            with self.subTest(kind=kind, value=value):
                self.native_field(kind, value)
                lease = self.native_prepare()
                self.assertTrue(self.request_input('start', lease=lease)['ok'])
                self.assertTrue(self.request_input('check', lease=lease)['ok'])
                self.assertTrue(self.request_input('cancel', lease=lease)['ok'])
                self.assertEqual(self.main_world('nativeField.value'), value)
                lease = self.native_prepare()
                self.assertTrue(self.request_input('start', lease=lease)['ok'])
                self.assertFalse(self.request_input('start', lease=lease)['ok'])
                self.assertTrue(self.request_input('commit', lease=lease, text=replacement)['ok'])
                self.assertEqual(self.main_world('nativeField.value'), replacement)
                self.assertEqual(self.main_world('second.value'), 'second')
                self.assertTrue(self.main_world("document.execCommand('undo')"))
                self.assertEqual(self.main_world('nativeField.value'), value)
                self.assertTrue(self.main_world("document.execCommand('redo')"))
                self.assertEqual(self.main_world('nativeField.value'), replacement)
                self.main_world('(nativeField.remove(),true)')

    def test_native_caret_does_not_trust_page_selection_prototype(self):
        self.native_field('email', 'public@example.test')
        self.main_world("(window.oldNativeSelection=ShadowRoot.prototype.getSelection,ShadowRoot.prototype.getSelection=()=>({rangeCount:1,anchorNode:document.body,focusNode:document.body,anchorOffset:0,focusOffset:0}),true)")
        try:
            lease = self.native_prepare()
            self.assertTrue(self.request_input('start', lease=lease)['ok'])
            self.assertTrue(self.request_input('commit', lease=lease, text='safe@example.test')['ok'])
            self.assertEqual(self.main_world('nativeField.value'), 'safe@example.test')
            self.unchanged()
        finally:
            self.main_world('(ShadowRoot.prototype.getSelection=oldNativeSelection,nativeField.remove(),true)')

    def test_native_capability_preserves_sensitive_field_refusal_and_recovery(self):
        for marker in ['one-time-code', 'cc-number', 'current-password', 'new-password']:
            with self.subTest(marker=marker):
                self.native_field('email', 'public@example.test')
                self.main_world('(nativeField.autocomplete=' + json.dumps(marker) + ',true)')
                result = self.request_input('prepare', native_caret=True)
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['code'], 'secret_field')
                self.assertEqual(self.main_world('nativeField.value'), 'public@example.test')
                self.main_world('(nativeField.autocomplete="",true)')
                lease = self.native_prepare()
                self.assertTrue(self.request_input('cancel', lease=lease)['ok'])
                self.main_world('(nativeField.remove(),true)')

    def test_native_email_beforeinput_cannot_lower_maxlength_and_truncate_text(self):
        self.native_field('email', 'public@example.test')
        lease = self.native_prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.main_world("(nativeField.addEventListener('beforeinput',()=>nativeField.maxLength=2,{once:true}),true)")
        result = self.request_input('commit', lease=lease, text='safe@example.test')
        self.assertFalse(result['ok'], result)
        self.assertEqual(self.main_world('nativeField.value'), 'public@example.test')
        self.unchanged()

    def test_native_number_rejects_filtered_text_without_deleting_original(self):
        for text in ['forty two', '1e999', '٧', '1.2.3']:
            with self.subTest(text=text):
                self.native_field('number', '123.5')
                lease = self.native_prepare()
                self.assertTrue(self.request_input('start', lease=lease)['ok'])
                result = self.request_input('commit', lease=lease, text=text)
                self.assertFalse(result['ok'], result)
                self.assertEqual(result['code'], 'rejected')
                self.assertEqual(self.main_world('nativeField.value'), '123.5')
                self.assertEqual(self.main_world('second.value'), 'second')
                self.main_world('(nativeField.remove(),true)')
        self.native_field('number', '123.5')
        lease = self.native_prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='-4.25e2')['ok'])
        self.assertEqual(self.main_world('Number(nativeField.value)'), -425)

    def test_native_fields_inside_original_iframe_and_shadow_host(self):
        for context in ['iframe', 'shadow']:
            if context == 'iframe':
                self.frame_field()
            for kind, value, replacement in [('email', 'public@example.test', '日本語@example.test'),
                                              ('number', '123.5', '42.75')]:
                with self.subTest(context=context, kind=kind):
                    doc = 'editorFrame.contentDocument' if context == 'iframe' else 'document'
                    host = doc + '.body' if context == 'iframe' else 'shadow.shadowRoot'
                    self.main_world("(()=>{const e=" + doc + ".createElement('input');e.id='nestedNative';e.type=" + json.dumps(kind) + ";e.value=" + json.dumps(value) + ";" + host + ".append(e);e.focus();e.select();return true})()")
                    lease = self.native_prepare()
                    self.assertTrue(self.request_input('start', lease=lease)['ok'])
                    self.assertTrue(self.request_input('commit', lease=lease, text=replacement)['ok'])
                    target = host + ".querySelector('#nestedNative')"
                    self.assertEqual(self.main_world(target + '.value'), replacement)
                    self.assertTrue(self.main_world(doc + ".execCommand('undo')"))
                    self.assertEqual(self.main_world(target + '.value'), value)
                    self.unchanged()
                    self.main_world('(' + target + '.remove(),true)')
            if context == 'iframe':
                self.main_world('(editorFrame.remove(),true)')

    def test_native_fields_refuse_focus_readonly_and_beforeinput_caret_changes(self):
        for kind, value in [('email', 'public@example.test'), ('number', '123.5')]:
            for attack in ['focus', 'readonly', 'caret']:
                with self.subTest(kind=kind, attack=attack):
                    self.native_field(kind, value)
                    lease = self.native_prepare()
                    self.assertTrue(self.request_input('start', lease=lease)['ok'])
                    if attack == 'focus':
                        self.main_world('(second.focus(),true)')
                    elif attack == 'readonly':
                        self.main_world('(nativeField.readOnly=true,true)')
                    else:
                        self.main_world("(window.changeNativeCaret=()=>document.getSelection().modify('move','forward','character'),nativeField.addEventListener('beforeinput',changeNativeCaret,{once:true}),true)")
                    self.assertFalse(self.request_input('commit', lease=lease, text='7')['ok'])
                    self.assertEqual(self.main_world('nativeField.value'), value)
                    self.assertEqual(self.main_world('second.value'), 'second')
                    self.main_world('(nativeField.remove(),true)')

    def test_focused_iframe_original_ack_unicode_and_native_undo(self):
        self.frame_field()
        lease = self.prepare()
        self.assertEqual(self.request_input('start', lease=lease)['data'], {'lease': lease, 'multiline': False})
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
            self.assertEqual(self.request_input('check', lease=lease)['data'], {'lease': lease, 'multiline': False})
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
        self.assertEqual(self.request_input('check', lease=lease)['data'], {'lease': lease, 'multiline': False})
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
        self.assertEqual(self.request_input('start', lease=lease)['data'], {'lease': lease, 'multiline': False})
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
        self.assertEqual(self.request_input('check', lease=lease)['data'], {'lease': lease, 'multiline': False})
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
            self.assertEqual(set(result['data']), {'lease', 'multiline'}, 'history exports no field content')
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
        self.assertEqual(self.request_input('start', lease=lease)['data'], {'lease': lease, 'multiline': False})
        self.assertTrue(self.request_input('commit', lease=lease, text='recovered')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha recovered gamma')
        self.assertEqual(self.main_world('second.value'), 'second')

    def test_document_open_cannot_reuse_removed_composition_observer(self):
        self.main_world("(()=>{document.open();document.write('<input id=first value=public><input id=second value=second>');document.close();first.focus();return true})()")
        result = self.request_input('prepare')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_composition')
        self.assertEqual(self.main_world('first.value'), 'public')
        self.assertEqual(self.main_world('second.value'), 'second')

    def test_composition_before_first_private_prepare_is_refused_and_recovers(self):
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true})),true)")
        result = self.request_input('prepare')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_composition')
        self.unchanged()
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),true)")
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('cancel', lease=lease)['ok'])
        self.unchanged()

    def test_preexisting_composition_blocks_successor_focus_until_it_ends(self):
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true})),second.focus(),true)")
        self.assertFalse(self.request_input('prepare')['ok'])
        self.unchanged()
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),first.focus(),first.setSelectionRange(6,10),true)")
        lease = self.prepare()
        self.assertTrue(self.request_input('cancel', lease=lease)['ok'])
        self.unchanged()

    def test_unrelated_custom_composition_end_cannot_release_the_original_field(self):
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true})),second.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),true)")
        result = self.request_input('prepare')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_composition')
        self.unchanged()
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),true)")
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.assertTrue(self.request_input('commit', lease=lease, text='日本語 café')['ok'])
        self.assertEqual(self.main_world('first.value'), 'alpha 日本語 café gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.unchanged()
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('first.value'), 'alpha 日本語 café gamma')

    def test_removed_custom_composition_editor_allows_only_a_fresh_successor(self):
        old = self.prepare()
        self.assertTrue(self.request_input('start', lease=old)['ok'])
        self.main_world("(window.removedEditor=first,first.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true})),first.remove(),second.focus(),second.setSelectionRange(0,6),true)")
        self.assertFalse(self.request_input('commit', lease=old, text='must not retarget')['ok'])
        fresh = self.prepare()
        self.assertNotEqual(fresh, old)
        self.assertTrue(self.request_input('start', lease=fresh)['ok'])
        self.request_input('cancel', lease=old)
        self.assertTrue(self.request_input('check', lease=fresh)['ok'])
        self.assertTrue(self.request_input('cancel', lease=fresh)['ok'])
        self.assertEqual(self.main_world('second.value'), 'second')
        fresh = self.prepare()
        self.assertTrue(self.request_input('start', lease=fresh)['ok'])
        self.assertTrue(self.request_input('commit', lease=fresh, text='東京 café')['ok'])
        self.assertEqual(self.main_world('second.value'), '東京 café')
        self.assertEqual(self.main_world('removedEditor.value'), 'alpha beta gamma')
        self.assertTrue(self.main_world("document.execCommand('undo')"))
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertTrue(self.main_world("document.execCommand('redo')"))
        self.assertEqual(self.main_world('second.value'), '東京 café')

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
