"""Remote-only actual pinned CEF renderer mutation regression matrix.

Run inside the unchanged native_composition.py private namespace fixture.
No production debugger port, hardware, network, or user browser is touched.
"""
import sys
import unittest
import native_composition as native

class RendererMutation(native.NativeComposition):
    def install(self, kind, statement):
        self.main_world('(window.rendererEvents=[],["compositionstart","compositionupdate","beforeinput","textInput","input","compositionend"].forEach(k=>first.addEventListener(k,e=>rendererEvents.push([k,e.isTrusted,e.cancelable]))),true)')
        self.main_world('(first.addEventListener(' + __import__('json').dumps(kind) + ',e=>{' + statement + '},{once:true}),true)')

    def cancel_direct(self):
        self.cdp.call('Input.imeSetComposition', text='', selectionStart=0, selectionEnd=0)

    def assert_replacement(self):
        self.assertEqual(self.main_world('first.value'), 'newer', self.main_world('rendererEvents'))
        self.assertEqual(self.main_world('[first.selectionStart,first.selectionEnd]'), [0,5])

    def test_renderer_beforeinput_value_cancel(self):
        self.preedit('候補')
        self.install('beforeinput', 'first.value="newer";first.setSelectionRange(0,5);e.preventDefault()')
        self.cancel_direct()
        self.assert_replacement()
        self.assertIn(['beforeinput', True, False], self.main_world('rendererEvents'))

    def test_renderer_compositionupdate_value_cancel(self):
        self.preedit('候補')
        self.install('compositionupdate', 'first.value="newer";first.setSelectionRange(0,5)')
        self.cancel_direct()
        self.assert_replacement()

    def test_renderer_beforeinput_value_commit(self):
        self.preedit('候補')
        self.install('beforeinput', 'first.value="newer";first.setSelectionRange(0,5)')
        self.cdp.call('Input.insertText', text='final')
        self.assert_replacement()

    def test_renderer_beforeinput_value_update(self):
        self.preedit('候補')
        self.install('beforeinput', 'first.value="newer";first.setSelectionRange(0,5)')
        self.preedit('updated')
        self.assert_replacement()

    def test_renderer_beforeinput_selection_cancel(self):
        self.preedit('候補')
        before=self.main_world('first.value')
        self.install('beforeinput', 'first.setSelectionRange(0,5)')
        self.cancel_direct()
        self.assertEqual(self.main_world('first.value'), before)
        self.assertEqual(self.main_world('[first.selectionStart,first.selectionEnd]'), [0,5])

    def test_renderer_beforeinput_focus_cancel(self):
        self.preedit('候補')
        before=self.main_world('first.value')
        self.install('beforeinput', 'second.focus();second.setSelectionRange(0,6)')
        self.cancel_direct()
        self.assertEqual(self.main_world('first.value'), before)
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertTrue(self.main_world('document.activeElement===second'))
        self.assertEqual(self.main_world('[second.selectionStart,second.selectionEnd]'), [0,6])

    def test_renderer_beforeinput_undo_cancel(self):
        self.preedit('候補')
        self.install('beforeinput', 'document.execCommand("undo")')
        self.cancel_direct()
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')

    def test_renderer_repeated_actions_after_replacement(self):
        self.preedit('候補')
        self.install('beforeinput', 'first.value="newer";first.setSelectionRange(0,5)')
        self.cancel_direct()
        self.assert_replacement()
        self.cancel_direct()
        self.assert_replacement()
        self.main_world('(first.setSelectionRange(5,5),true)')
        self.preedit('next')
        self.cdp.call('Input.insertText', text='done')
        self.assertEqual(self.main_world('first.value'), 'newerdone')

    def test_renderer_compositionstart_value_preserved(self):
        self.install('compositionstart', 'first.value="newer";first.setSelectionRange(0,5)')
        self.preedit('candidate')
        self.assert_replacement()

    def test_renderer_compositionstart_selection_preserved(self):
        self.install('compositionstart', 'first.setSelectionRange(0,5)')
        self.preedit('candidate')
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        self.assertEqual(self.main_world('[first.selectionStart,first.selectionEnd]'), [0,5])

    def test_renderer_compositionstart_focus_preserved(self):
        self.install('compositionstart', 'second.focus();second.setSelectionRange(0,6)')
        self.preedit('candidate')
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertTrue(self.main_world('document.activeElement===second'))
        self.assertEqual(self.main_world('[second.selectionStart,second.selectionEnd]'), [0,6])

    def test_renderer_textinput_commit_value_preserved(self):
        self.preedit('候補')
        self.install('textInput', 'first.value="newer";first.setSelectionRange(0,5)')
        self.cdp.call('Input.insertText', text='final')
        self.assertIn('textInput', self.main_world('rendererEvents.map(e=>e[0])'))
        self.assert_replacement()

    def test_renderer_textinput_commit_focus_preserved(self):
        self.preedit('候補')
        self.install('textInput', 'second.focus();second.setSelectionRange(0,6)')
        self.cdp.call('Input.insertText', text='final')
        self.assertIn('textInput', self.main_world('rendererEvents.map(e=>e[0])'))
        self.assertEqual(self.main_world('second.value'), 'second')
        self.assertTrue(self.main_world('document.activeElement===second'))
        self.assertEqual(self.main_world('[second.selectionStart,second.selectionEnd]'), [0,6])

    def test_renderer_idle_beforeinput_replacement_preserved(self):
        self.install('beforeinput', 'first.value="newer";first.setSelectionRange(0,5)')
        self.cancel_direct()
        self.assert_replacement()
        self.cancel_direct()
        self.assert_replacement()

    def test_renderer_ordinary_cancel_event_semantics(self):
        self.preedit('候補')
        self.install('beforeinput', 'e.preventDefault()')
        self.cancel_direct()
        self.assertEqual(self.main_world('first.value'), 'alpha gamma')
        self.assertEqual(self.main_world('rendererEvents.filter(e=>e[0]!=="textInput").map(e=>e[0])'), ['compositionupdate','beforeinput','input','compositionend'])
        self.assertEqual(self.main_world('rendererEvents.filter(e=>e[0]!=="textInput")'), [['compositionupdate',True,True],['beforeinput',True,False],['input',True,False],['compositionend',False,True]])

    def test_renderer_style_handler_commit_and_undo(self):
        self.preedit('候補')
        self.install('beforeinput', 'first.style.color="red";document.title="changed"')
        self.cdp.call('Input.insertText', text='final')
        self.assertEqual(self.main_world('first.value'), 'alpha final gamma')
        self.assertTrue(self.main_world('document.execCommand("undo")'))
        self.assertEqual(self.main_world('first.value'), 'alpha 候補 gamma')
        self.assertTrue(self.main_world('document.execCommand("undo")'))
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')

if __name__=='__main__':
    names=['RendererMutation.'+name for name in sorted(dir(RendererMutation)) if name.startswith('test_renderer_')]
    unittest.main(argv=[__file__, '-v', *names])
