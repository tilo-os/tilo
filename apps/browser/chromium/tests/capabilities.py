#!/usr/bin/env python3
"""Live CEF capability checks; uses actual IPC, network, and disk output."""
import http.server
import os
import json
import time
import threading
import unittest
from pathlib import Path
from native_regressions import Host

PAYLOAD = b'Tilo download fixture\n'
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        if self.path == '/download':
            self.send_header('Content-Disposition', 'attachment; filename="../../fixture.txt"')
            self.send_header('Content-Type', 'application/octet-stream')
            self.end_headers()
            self.wfile.write(PAYLOAD)
        else:
            self.send_header('Content-Type', 'text/html')
            self.end_headers()
            self.wfile.write(b'''<title>permission fixture</title><a style="position:absolute;top:50px;left:10px" href="/download">Download</a><button onclick="navigator.geolocation.getCurrentPosition(()=>document.title='granted',e=>document.title='denied-'+e.code)">Location</button>''')
    def log_message(self, *args): pass

MEDIA_TESTS={'test_fake_camera_microphone_policy_grant_revoke_and_new_request','test_refused_camera_is_kept_until_the_site_may_ask_again'}

class Capabilities(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=cls.server.serve_forever,daemon=True).start()
        cls.url=f'http://127.0.0.1:{cls.server.server_port}'
    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown();cls.server.server_close()
    def setUp(self):
        self.h=Host(args=['--use-fake-device-for-media-stream'] if self._testMethodName in MEDIA_TESTS else []);self.addCleanup(self.h.close);self.addCleanup(self.save_evidence)
    def save_evidence(self):
        if directory:=os.environ.get('TILO_SITE_EVIDENCE_DIR'):
            path=Path(directory);path.mkdir(parents=True,exist_ok=True)
            (path/(self._testMethodName+'.json')).write_text(json.dumps({'events':self.h.seen[-2000:]},indent=2))
    def status(self,origin,request=1):
        self.h.send(op='site_status',request_id=request,origin=origin)
        return self.h.until(lambda e:e['event']=='site_status' and e['request_id']==request)
    def query(self,expression):
        self.h.send(op='scripts',scripts=["document.title=JSON.stringify({value:("+expression+"),nonce:"+str(time.monotonic_ns())+"})"])
        return json.loads(self.h.until(lambda e:e['event']=='title' and e['title'].startswith('{'))['title'])['value']
    def test_origin_scoped_clear_preserves_other_host_and_rejects_stale_document(self):
        other=self.url.replace('127.0.0.1','localhost')
        for origin in [self.url,other]:
            self.h.navigate(origin+'/')
            self.query("(()=>{localStorage.setItem('fixture','synthetic');document.cookie='fixture=synthetic;path=/';return true})()")
            self.h.send(op='scripts',scripts=[])
        self.h.navigate(self.url+'/')
        state=self.status(self.url)
        self.h.send(op='site_action',request_id=2,origin=self.url,generation=state['generation'],action='clear')
        result=self.h.until(lambda e:e['event']=='site_result' and e['request_id']==2)
        self.assertTrue(result['success'],result)
        self.h.navigate(self.url+'/')
        self.assertEqual(self.query("[localStorage.getItem('fixture'),document.cookie]"),[None,''])
        self.h.send(op='scripts',scripts=[])
        self.h.navigate(other+'/')
        self.assertEqual(self.query("[localStorage.getItem('fixture'),document.cookie]"),['synthetic','fixture=synthetic'])
        self.h.send(op='site_action',request_id=3,origin=self.url,generation=state['generation'],action='clear')
        stale=self.h.until(lambda e:e['event']=='site_result' and e['request_id']==3)
        self.assertFalse(stale['success'])
        self.assertEqual(self.query("localStorage.getItem('fixture')"),'synthetic')
    def test_untrusted_tls_remains_blocked(self):
        import ssl,subprocess,tempfile
        with tempfile.TemporaryDirectory(prefix='tilo-site-tls-') as directory:
            key=Path(directory)/'key.pem';certificate=Path(directory)/'cert.pem'
            subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(key),'-out',str(certificate),'-days','1','-subj','/CN=localhost'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
            context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(certificate,key);server.socket=context.wrap_socket(server.socket,server_side=True)
            threading.Thread(target=server.serve_forever,daemon=True).start()
            try:
                url=f'https://localhost:{server.server_port}/'
                self.h.send(op='navigate',url=url)
                error=self.h.until(lambda e:e['event']=='error' and e.get('url')==url)
                self.assertIn('CERT',error['message'])
                self.assertFalse(any(e['event']=='loaded' and e.get('url')==url and e.get('status')==200 for e in self.h.seen))
            finally:server.shutdown();server.server_close()

    def test_fake_camera_microphone_policy_grant_revoke_and_new_request(self):
        def request(marker):
            self.h.send(op='scripts',scripts=["navigator.mediaDevices.getUserMedia({video:true,audio:true}).then(s=>{s.getTracks().forEach(t=>t.stop());document.title='"+marker+"-granted'}).catch(e=>document.title='"+marker+"-'+e.name)"])
        self.h.navigate(self.url+'/')
        request('first')
        prompt=self.h.until(lambda e:e['event']=='permission_prompt')
        self.assertIn('camera',prompt['permissions']);self.assertIn('microphone',prompt['permissions'])
        self.h.send(op='permission_decision',id=prompt['id'],allow=True)
        self.h.until(lambda e:e['event']=='title' and e['title']=='first-granted')
        state=self.status(self.url)
        self.assertEqual(state['permissions']['camera'],'allowed')
        self.assertEqual(state['permissions']['microphone'],'allowed')
        request('remembered')
        remembered=self.h.until(lambda e:e['event']=='permission_prompt' or (e['event']=='title' and e['title'].startswith('remembered-')))
        self.assertEqual(remembered.get('title'),'remembered-granted',remembered)
        self.h.send(op='scripts',scripts=[])
        other=self.url.replace('127.0.0.1','localhost')
        self.h.navigate(other+'/')
        other_state=self.status(other,3)
        self.assertEqual(other_state['permissions']['camera'],'ask')
        self.assertEqual(other_state['permissions']['microphone'],'ask')
        request('other')
        other_prompt=self.h.until(lambda e:e['event']=='permission_prompt')
        self.h.send(op='permission_decision',id=other_prompt['id'],allow=False)
        self.h.until(lambda e:e['event']=='title' and e['title']=='other-NotAllowedError')
        self.h.send(op='scripts',scripts=[])
        self.h.navigate(self.url+'/')
        state=self.status(self.url,4)
        self.h.send(op='site_action',request_id=5,origin=self.url,generation=state['generation'],action='revoke')
        self.assertTrue(self.h.until(lambda e:e['event']=='site_result' and e['request_id']==5)['success'])
        state=self.status(self.url,6)
        self.assertEqual(state['permissions']['camera'],'blocked')
        self.assertEqual(state['permissions']['microphone'],'blocked')
        after=len(self.h.seen)
        request('after-revoke')
        outcome=self.h.until(lambda e:e['event']=='permission_prompt' or (e['event']=='title' and e['title'].startswith('after-revoke-')))
        self.assertEqual(outcome.get('title'),'after-revoke-NotAllowedError',outcome)
        self.h.settle(.2)
        self.assertFalse(any(e['event']=='permission_prompt' for e in self.h.seen[after:]))

    def request_media(self,marker):
        self.h.send(op='scripts',scripts=["navigator.mediaDevices.getUserMedia({video:true,audio:true}).then(s=>{s.getTracks().forEach(t=>t.stop());document.title='"+marker+"-granted'}).catch(e=>document.title='"+marker+"-'+e.name)"])
    def media_outcome(self,marker):
        return self.h.until(lambda e:e['event']=='permission_prompt' or (e['event']=='title' and e['title'].startswith(marker+'-')))

    def test_refused_camera_is_kept_until_the_site_may_ask_again(self):
        # Refusing the camera is remembered as refusing a location is: the
        # site cannot ask again on every visit, and site information says so.
        self.h.navigate(self.url+'/')
        self.request_media('refused')
        prompt=self.h.until(lambda e:e['event']=='permission_prompt')
        self.h.send(op='permission_decision',id=prompt['id'],allow=False)
        self.h.until(lambda e:e['event']=='title' and e['title']=='refused-NotAllowedError')
        state=self.status(self.url)
        self.assertEqual(state['permissions']['camera'],'blocked')
        self.assertEqual(state['permissions']['microphone'],'blocked')
        self.h.send(op='scripts',scripts=[])
        self.h.navigate(self.url+'/?again')
        self.request_media('again')
        self.assertEqual(self.media_outcome('again').get('title'),'again-NotAllowedError')
        state=self.status(self.url,2)
        self.h.send(op='site_action',request_id=3,origin=self.url,generation=state['generation'],action='reset')
        self.assertTrue(self.h.until(lambda e:e['event']=='site_result' and e['request_id']==3)['success'])
        state=self.status(self.url,4)
        self.assertEqual(state['permissions']['camera'],'ask')
        self.assertEqual(state['permissions']['microphone'],'ask')
        self.h.send(op='scripts',scripts=[])
        self.h.navigate(self.url+'/?reset')
        self.request_media('reset')
        self.assertEqual(self.media_outcome('reset')['event'],'permission_prompt')

    def ask_notifications(self,marker):
        self.h.send(op='scripts',scripts=["Notification.requestPermission().then(p=>document.title='"+marker+"-'+p)"])
        outcome=self.h.until(lambda e:e['event']=='permission_prompt' or (e['event']=='title' and e['title'].startswith(marker+'-')))
        self.h.send(op='scripts',scripts=[])
        return outcome

    def test_unanswered_question_is_no_refusal_when_the_page_moves_on(self):
        # Leaving a page, or reloading it, while it asks withdraws the
        # question; Chromium must not keep it as a block for the site.
        for request,step in enumerate(['leave','reload'],start=1):
            page=self.url+'/?ask-'+step
            self.h.navigate(page)
            prompt=self.ask_notifications(step)
            self.assertEqual(prompt.get('permissions'),'notifications',prompt)
            if step=='reload':
                self.h.send(op='reload')
                self.h.until(lambda e:e['event']=='loaded' and e['url']==page)
                self.h.settle()
            else:self.h.navigate(self.url+'/?left')
            closed=lambda e:e['event']=='permission_closed' and e['id']==prompt['id']
            if not any(closed(e) for e in self.h.seen):self.h.until(closed)
            self.assertEqual(self.status(self.url,request)['permissions']['notifications'],'ask',step)
        self.assertEqual(self.ask_notifications('again')['event'],'permission_prompt')

    def test_ask_again_lets_a_site_ask_after_questions_set_aside(self):
        # A few questions set aside make Chromium refuse the site without
        # asking; "Ask again" must undo that too, not only blocks and grants.
        refused=None
        for attempt in range(8):
            self.h.navigate(self.url+f'/?aside-{attempt}')
            outcome=self.ask_notifications(f'aside-{attempt}')
            if outcome['event']!='permission_prompt':
                self.assertGreater(attempt,0,'the site was asked at first')
                refused=outcome;break
            self.h.send(op='permission_decision',id=outcome['id'],allow=False,dismiss=True)
            self.h.until(lambda e:e['event']=='title' and e['title'].startswith(f'aside-{attempt}-'))
        self.assertTrue(refused and refused['title'].endswith('-denied'),refused)
        state=self.status(self.url)
        self.assertEqual(state['permissions']['notifications'],'ask')
        self.h.send(op='site_action',request_id=2,origin=self.url,generation=state['generation'],action='reset')
        self.assertTrue(self.h.until(lambda e:e['event']=='site_result' and e['request_id']==2)['success'])
        self.h.navigate(self.url+'/?after-reset')
        self.assertEqual(self.ask_notifications('after-reset')['event'],'permission_prompt')

    def test_synthetic_notification_grant_then_revoke(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='scripts',scripts=["document.body.innerHTML='<button style=position:absolute;top:0;left:0;width:200px;height:80px onclick=Notification.requestPermission()>Allow notification fixture</button>'"])
        self.h.settle(.2)
        self.h.send(op='mouse_button',button='left',down=True,x=40,y=30)
        self.h.send(op='mouse_button',button='left',down=False,x=40,y=30)
        prompt=self.h.until(lambda e:e['event']=='permission_prompt')
        self.assertIn('notifications',prompt['permissions'])
        self.h.send(op='permission_decision',id=prompt['id'],allow=True)
        self.h.settle(.2)
        status=self.status(self.url)
        self.assertEqual(status['permissions']['notifications'],'allowed')
        self.h.send(op='site_action',request_id=2,origin=self.url,generation=status['generation'],action='revoke')
        self.assertTrue(self.h.until(lambda e:e['event']=='site_result' and e['request_id']==2)['success'])
        self.assertEqual(self.status(self.url,3)['permissions']['notifications'],'blocked')
        self.assertEqual(self.query('Notification.permission'),'denied')

    def test_download_waits_for_accept_and_saves_exact_bytes(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=58)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=58)
        event=self.h.until(lambda e:e['event']=='download_prompt',timeout=5)
        self.assertNotIn('/',event['name'])
        name=event['name']
        self.h.send(op='download_decision',id=event['id'],allow=True)
        done=self.h.until(lambda e:e['event']=='download' and e['state']=='complete')
        saved=Path(done['path'])
        self.assertEqual(saved.read_bytes(),PAYLOAD)
        self.assertEqual(saved.name,name)
        # Each accepted download has an exclusive directory, never replaces a file.
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=58)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=58)
        event=self.h.until(lambda e:e['event']=='download_prompt')
        self.h.send(op='download_decision',id=event['id'],allow=True)
        other=self.h.until(lambda e:e['event']=='download' and e['state']=='complete')
        self.assertNotEqual(other['path'],str(saved))
        self.assertEqual(saved.read_bytes(),PAYLOAD)
    def test_declined_download_is_cancelled_without_a_saved_file(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=58)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=58)
        prompt=self.h.until(lambda e:e['event']=='download_prompt')
        self.h.send(op='download_decision',id=prompt['id'],allow=False)
        result=self.h.until(lambda e:e['event']=='download' and e['state']=='cancelled')
        self.assertFalse(result['path'])
        self.assertFalse((Path(self.h.profile.name)/'downloads').exists())
    def test_clear_on_exit_removes_data_before_next_startup(self):
        self.h.navigate(self.url+'/')
        root=Path(self.h.profile.name)/'default'
        root.mkdir(exist_ok=True)
        (root/'exit-clear-fixture').write_text('private')
        self.h.send(op='clear_on_exit',enabled=True)
        self.h.send(op='shutdown')
        self.h.until(lambda e:e['event']=='closed')
        self.assertEqual(self.h.p.wait(timeout=35),0)
        self.assertFalse(root.exists())
        # close() would send another shutdown, so only release fixture resources.
        self._cleanups.clear()
        self.h.p.stdin.close();self.h.p.stdout.close();self.h.log.close();self.h.profile.cleanup()
    def test_permission_denied_only_after_explicit_decision(self):
        self.h.navigate(self.url+'/')
        self.h.send(op='mouse_button',button='left',down=True,x=35,y=18)
        self.h.send(op='mouse_button',button='left',down=False,x=35,y=18)
        prompt=self.h.until(lambda e:e['event']=='permission_prompt',timeout=5)
        self.assertIn('127.0.0.1',prompt['origin'])
        self.h.send(op='permission_decision',id=prompt['id'],allow=False)
        self.h.until(lambda e:e['event']=='title' and e['title']=='denied-1')

if __name__=='__main__': unittest.main(verbosity=2)
