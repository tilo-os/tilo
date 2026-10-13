#!/usr/bin/env python3
"""Actual Blink composition; run only in a private PID/network namespace."""
import base64
import hashlib
import http.client
import json
import os
import socket
import struct
import time
import unittest
import urllib.parse
import guarded_text_input as guarded
from native_regressions import Host

class CDP:
    def __init__(self, url):
        u=urllib.parse.urlsplit(url); self.sock=socket.create_connection((u.hostname,u.port),5);self.sock.settimeout(5);self.pending=b'';self.sequence=0
        key=base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f'GET {u.path} HTTP/1.1\r\nHost: {u.hostname}:{u.port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
        while b'\r\n\r\n' not in self.pending:
            self.pending+=self.sock.recv(4096)
            if len(self.pending)>16384:raise RuntimeError('bounded handshake failed')
        header,self.pending=self.pending.split(b'\r\n\r\n',1)
        assert header.startswith(b'HTTP/1.1 101'),header[:120]
        expected=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        assert expected in header
    def exact(self,n):
        while len(self.pending)<n:
            data=self.sock.recv(min(65536,n-len(self.pending)))
            if not data:raise EOFError('owned inspector closed')
            self.pending+=data
        result,self.pending=self.pending[:n],self.pending[n:];return result
    def send(self,payload,op=1):
        assert len(payload)<65536
        length=bytes([len(payload)|128]) if len(payload)<126 else b'\xfe'+struct.pack('!H',len(payload))
        mask=os.urandom(4)
        self.sock.sendall(bytes([128|op])+length+mask+bytes(v^mask[i%4] for i,v in enumerate(payload)))
    def call(self,method,**params):
        self.sequence+=1;seq=self.sequence;self.send(json.dumps({'id':seq,'method':method,'params':params}).encode())
        while True:
            head=self.exact(2);assert head[0]&128;size=head[1]&127
            if size==126:size=struct.unpack('!H',self.exact(2))[0]
            elif size==127:size=struct.unpack('!Q',self.exact(8))[0]
            assert size<=1024*1024
            mask=self.exact(4) if head[1]&128 else None;payload=self.exact(size)
            if mask:payload=bytes(v^mask[i%4] for i,v in enumerate(payload))
            op=head[0]&15
            if op==9:self.send(payload,10);continue
            if op==8:raise EOFError('owned inspector closed')
            assert op==1
            value=json.loads(payload)
            if value.get('id')!=seq:continue
            if 'error' in value:raise RuntimeError(value['error'])
            return value['result']
    def close(self):self.sock.close()

class NativeComposition(unittest.TestCase):
    setUpClass = classmethod(guarded.GuardedTextInput.setUpClass.__func__)
    tearDownClass = classmethod(guarded.GuardedTextInput.tearDownClass.__func__)
    main_world = guarded.GuardedTextInput.main_world
    request_input = guarded.GuardedTextInput.request_input
    prepare = guarded.GuardedTextInput.prepare

    def setUp(self):
        self.assertEqual(os.environ.get('TILO_TEST_PRIVATE_NAMESPACE'), '1',
                         'actual composition probes require the owned private namespace')
        self.h = Host(args=('--remote-debugging-port=9333',))
        self.addCleanup(self.h.close)
        self.h.send(op='visibility', visible=True)
        self.h.send(op='resize', width=1000, height=800, scale_factor=1.0)
        self.h.navigate(self.url)
        self.h.send(op='focus', focused=True)
        self.request = 0
        deadline = time.monotonic() + 10
        while not self.main_world('(first.focus(),first.setSelectionRange(6,10),document.hasFocus())'):
            self.assertLess(time.monotonic(), deadline, 'native document must acquire focus')
            time.sleep(.02)
        conn = http.client.HTTPConnection('127.0.0.1', 9333, timeout=5)
        conn.request('GET', '/json/list')
        response = conn.getresponse()
        self.assertEqual(response.status, 200)
        targets = json.loads(response.read(65536))
        conn.close()
        target = next(t for t in targets if t['url'] == self.url)
        self.cdp = CDP(target['webSocketDebuggerUrl'])
        self.addCleanup(self.cdp.close)

    def focus_first(self):
        self.h.send(op='focus', focused=True)
        deadline = time.monotonic() + 10
        while not self.main_world('(first.focus(),first.setSelectionRange(6,10),document.hasFocus())'):
            self.assertLess(time.monotonic(), deadline, 'fresh original document must acquire focus')
            time.sleep(.02)

    def preedit(self, text):
        self.cdp.call('Input.imeSetComposition', text=text, selectionStart=0,
                      selectionEnd=len(text))

    def test_native_composition_before_first_prepare_and_forged_end(self):
        self.main_world("(window.nativeCompositionEvents=[],['compositionstart','compositionend'].forEach(kind=>addEventListener(kind,e=>nativeCompositionEvents.push([kind,e.isTrusted]))),true)")
        self.preedit('候補')
        value = self.main_world('first.value')
        self.assertIn('候補', value, 'stimulus must create actual Blink preedit')
        result = self.request_input('prepare')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_composition')
        self.main_world("(first.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),true)")
        self.assertFalse(self.request_input('prepare')['ok'], 'page end cannot clear native composition')
        self.assertEqual(self.main_world('first.value'), value)
        self.assertEqual(self.main_world('second.value'), 'second')
        self.cdp.call('Input.insertText', text='確定')
        self.assertEqual(self.main_world('first.value'), 'alpha 確定 gamma',
                         'Actual native completion must commit exactly its preedit replacement')
        events = self.main_world('nativeCompositionEvents')
        self.assertEqual(events, [['compositionstart', True],
                                  ['compositionend', False], ['compositionend', False]])
        self.assertFalse(self.request_input('prepare')['ok'],
                         'untrusted debugger completion cannot certify native idle')
        self.h.navigate(self.url + '?fresh-after-untrusted-end')
        self.focus_first()
        lease = self.prepare()
        self.assertTrue(self.request_input('cancel', lease=lease)['ok'])

    def test_native_preedit_before_first_prepare_in_initial_iframe(self):
        guarded.GuardedTextInput.frame_field(self)
        self.preedit('候補')
        expression = "editorFrame.contentDocument.getElementById('first').value"
        value = self.main_world(expression)
        self.assertIn('候補', value, 'stimulus must reach the actual iframe editor')
        result = self.request_input('prepare')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_composition')
        self.main_world("(editorFrame.contentDocument.getElementById('first').dispatchEvent(new CompositionEvent('compositionend',{bubbles:true})),true)")
        self.assertFalse(self.request_input('prepare')['ok'],
                         'a page end cannot clear native iframe preedit')
        self.assertEqual(self.main_world(expression), value)
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.h.navigate(self.url + '?fresh-after-iframe-composition')
        self.focus_first()
        lease = self.prepare()
        self.assertTrue(self.request_input('cancel', lease=lease)['ok'])

    def test_native_composition_retires_started_lease_across_focus_and_cancel(self):
        lease = self.prepare()
        self.assertTrue(self.request_input('start', lease=lease)['ok'])
        self.preedit('候補')
        self.assertFalse(self.request_input('check', lease=lease)['ok'])
        self.main_world('(second.focus(),true)')
        second = self.main_world('second.value')
        self.assertFalse(self.request_input('commit', lease=lease, text='must not insert')['ok'])
        self.assertEqual(self.main_world('second.value'), second)
        # Focus movement may end native composition without an observable end.
        # Retain refusal instead of assuming idle; a new document has a new
        # document-start observer and cannot revive the original lease.
        self.assertFalse(self.request_input('prepare')['ok'])
        self.h.navigate(self.url + '?fresh-after-focus')
        self.focus_first()
        self.assertFalse(self.request_input('commit', lease=lease, text='stale')['ok'])
        fresh = self.prepare()
        self.assertTrue(self.request_input('start', lease=fresh)['ok'])
        self.assertTrue(self.request_input('cancel', lease=fresh)['ok'])
        self.assertFalse(self.request_input('commit', lease=fresh, text='canceled')['ok'])
        self.assertEqual(self.main_world('second.value'), second)

    def test_missing_document_start_observer_refuses_without_field_mutation(self):
        frame = self.cdp.call('Page.getFrameTree')['frameTree']['frame']['id']
        world = self.cdp.call('Page.createIsolatedWorld', frameId=frame,
                              worldName='tilo-private-text-input')['executionContextId']
        self.cdp.call('Runtime.evaluate', contextId=world,
                      expression='delete globalThis.__tiloTextComposition')
        result = self.request_input('prepare')
        self.assertFalse(result['ok'], result)
        self.assertEqual(result['code'], 'text_input_composition')
        self.assertEqual(self.main_world('first.value'), 'alpha beta gamma')
        self.assertEqual(self.main_world('second.value'), 'second')
        self.h.navigate(self.url + '?fresh')
        self.focus_first()
        lease = self.prepare()
        self.assertTrue(self.request_input('cancel', lease=lease)['ok'])


if __name__ == '__main__':
    unittest.main(argv=[__file__, *(__import__('sys').argv[2:])])
