#!/usr/bin/env python3
"""Local HTTP integration tests: real sandboxed CEF + the native Rust matcher."""
import http.server
import os
from pathlib import Path
import json
import threading
import time
import unittest
from urllib.parse import urlsplit
from native_regressions import Host

PAGE = b'''<!doctype html><title>blocking fixture</title>
<script>window.earlyScript=window.docStartReady===true;setTimeout(()=>document.title="early-"+window.earlyScript,300)</script>
<style>.csp-control { display:none }</style><div class="csp-control">CSP control</div>
<div class="sponsor">advertisement</div><div id="domain-ad">domain advertisement</div>
<script src="/ad.js"></script><script src="/ad.js?allow"></script>
<script src="/replace.js" onload="window.redirectLoaded=true"></script>
<script src="/image-only.js"></script><script src="/first.js"></script>
<script>
fetch('/fetch-ad').catch(()=>{});
fetch('/empty-response').then(r=>r.text()).then(t=>window.emptyReplacement=t).catch(()=>{});
new Worker('/worker.js');
navigator.serviceWorker.register('/sw.js');
let third=document.createElement('script');third.src=location.href.replace('127.0.0.1','localhost')+'third.js';document.head.append(third);
setTimeout(()=>{let ad=document.createElement('div');ad.className='late-ad';document.body.append(ad)},150);
</script>'''
RULES = '\n'.join(['/ad.js$script', '@@/ad.js?allow$script', '/image-only.js$image',
                   '/first.js$third-party', '/third.js$third-party', '/sw-ad$xmlhttprequest', '/fetch-ad$xmlhttprequest', '/worker-ad$xmlhttprequest',
                   '##.sponsor', '##.late-ad', '127.0.0.1###domain-ad'])

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.requests.append(self.path)
        path=urlsplit(self.path).path
        self.send_response(200)
        self.send_header('Cache-Control','no-store')
        if getattr(self.server,'csp',False): self.send_header('Content-Security-Policy',"style-src 'none'; script-src 'self' 'unsafe-inline'")
        self.send_header('Content-Type','text/html' if path in ('/','/iframes','/csp','/site-parent','/site-child') else 'text/javascript')
        self.end_headers()
        body=b"self.addEventListener('install',e=>self.skipWaiting());self.addEventListener('activate',e=>e.waitUntil(fetch('/sw-ad').catch(()=>{}).then(()=>fetch('/sw-ready'))));" if path=='/sw.js' else PAGE if path=='/' else b"fetch('/worker-ad').catch(()=>{})" if path=='/worker.js' else b'window.originalReplacement=true;' if path=='/replace.js' else b'void 0;'
        if path=='/csp':
            body=b'''<!doctype html><style>.csp-control {display:none}</style>
<div class="csp-control">control</div><div class="site-sheet-control">site sheet</div><div class="sponsor">ad</div><div id="domain-ad">ad</div>
<script>
setTimeout(()=>{const node=document.createElement('div');node.className='late-ad';document.body.append(node)},100);
try { eval('window.pageEvalAllowed=true') } catch (_) { window.pageEvalAllowed=false }
const siteSheet=new CSSStyleSheet();siteSheet.replaceSync('.site-sheet-control {display:grid}');document.adoptedStyleSheets=[...document.adoptedStyleSheets,siteSheet];
setInterval(()=>{const late=document.querySelector('.late-ad');if(late)document.title=JSON.stringify({styles:['.sponsor','.late-ad','#domain-ad'].map(s=>getComputedStyle(document.querySelector(s)).display),control:getComputedStyle(document.querySelector('.csp-control')).display,site:getComputedStyle(document.querySelector('.site-sheet-control')).display,late:window.qosLateCount||0,pageEvalAllowed:window.pageEvalAllowed,nonce:Date.now()})},100);
</script>'''
        if path=='/iframes':
            body=b'<!doctype html><title>iframe source</title>'
            for host,name in [('localhost','thirdparty-frame'),('localhost','firstparty-frame'),('localhost','domain-frame'),('127.0.0.1','same-first-frame'),('127.0.0.1','same-third-frame')]:
                body+=f'<iframe src="http://{host}:{self.server.server_port}/{name}"></iframe>'.encode()
        if path in ('/site-parent','/site-child'):
            body=b'''<!doctype html><div class="site-ad">ad</div><script>
const blockedScript=document.createElement('script');blockedScript.src='/site-ad.js';document.head.append(blockedScript);
window.network=false;fetch('/site-ad-fetch').then(()=>window.network=true).catch(()=>{});
setInterval(()=>{const state={cosmetic:getComputedStyle(document.querySelector('.site-ad')).display,scriptlet:window.siteFixture===true,supplement:window.siteSupplement===true,network:window.network};if(window===top)document.title=JSON.stringify({main:state,child:window.childState||null,nonce:Date.now()});else parent.postMessage(state,'*')},100);
addEventListener('message',e=>window.childState=e.data);
</script>'''
            if path=='/site-parent':
                other='localhost' if self.headers['Host'].startswith('127.0.0.1') else '127.0.0.1'
                body+=f'<iframe src="http://{other}:{self.server.server_port}/site-child"></iframe>'.encode()
        self.wfile.write(body)
    def log_message(self,*args): pass

class BlockingRegressions(unittest.TestCase):
    def setUp(self):
        self.server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.server.requests=[]
        threading.Thread(target=self.server.serve_forever,daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.host=Host()
        self.addCleanup(self.host.close)
        self.url=f'http://127.0.0.1:{self.server.server_port}/'
        self.addCleanup(self.save_evidence)
    def save_evidence(self):
        if directory:=os.environ.get('QOS_SITE_EVIDENCE_DIR'):
            path=Path(directory);path.mkdir(parents=True,exist_ok=True)
            self.host.log.seek(0)
            (path/(self._testMethodName+'.stderr')).write_bytes(self.host.log.read())
            (path/(self._testMethodName+'.json')).write_text(json.dumps({'events':self.host.seen[-2000:],'requests':self.server.requests[-2000:]},indent=2))
    def config(self,enabled=True,rules=RULES):
        self.host.send(op='filter_rules',rules=rules,enabled=enabled)
    def loaded(self):
        self.host.navigate(self.url)
        self.host.settle(.5)
    def styles(self):
        self.host.send(op='scripts',scripts=["document.title=JSON.stringify({styles:['.sponsor','.late-ad','#domain-ad'].map(s=>getComputedStyle(document.querySelector(s)).display),nonce:"+str(time.monotonic_ns())+"})"])
        return json.loads(self.host.until(lambda e:e['event']=='title' and e['title'].startswith('{'))['title'])['styles']
    def site_status(self,request=1):
        self.host.send(op='site_status',request_id=request,origin=self.site_origin)
        return self.host.until(lambda e:e['event']=='site_status' and e['request_id']==request)
    def site_action(self,kind,request=2,origin=None,generation=None):
        status=self.site_status(request+100)
        self.host.send(op='site_action',request_id=request,origin=origin or self.site_origin,generation=generation or status['generation'],action=kind)
        return self.host.until(lambda e:e['event']=='site_result' and e['request_id']==request)
    def toggle_and_reload(self, kind):
        changed = self.site_action(kind)
        self.assertTrue(changed['success'], changed)
        self.host.send(op='site_action', request_id=20, origin=changed['origin'],
                       generation=changed['generation'], action='reload')
        reloaded = self.host.until(lambda event: event['event']=='site_result' and event['request_id']==20)
        self.assertTrue(reloaded['success'], reloaded)
        self.assertEqual(reloaded['generation'], changed['generation'])
        self.host.until(lambda event: event['event']=='loaded' and event['url']==self.site_origin+'/site-parent')
    def site_state(self):
        self.host.settle(.3)
        return json.loads(self.host.until(lambda e:e['event']=='title' and e['title'].startswith('{"main":') and json.loads(e['title'])['child'] is not None)['title'])
    def test_exact_top_origin_exception_covers_frames_and_all_blocking_layers(self):
        rules='/site-ad.js$script\n/site-ad-fetch$xmlhttprequest\n##.site-ad\n127.0.0.1##+js(set-constant, siteFixture, true)\nlocalhost##+js(set-constant, siteFixture, true)'
        self.config(rules=rules)
        self.host.send(op='blocking_scripts',scripts=['window.siteSupplement=true'])
        self.site_origin=self.url.rstrip('/')
        def verify(protected):
            state=self.site_state()
            for frame in ['main','child']:
                self.assertEqual(state[frame],{'cosmetic':'none' if protected else 'block','scriptlet':protected,'supplement':protected,'network':not protected},state)
            self.assertEqual(self.site_status()['blocked'],4 if protected else 0)
        self.host.navigate(self.site_origin+'/site-parent');verify(True)
        self.toggle_and_reload('disable');verify(False)
        self.site_origin=self.site_origin.replace('127.0.0.1','localhost')
        self.host.navigate(self.site_origin+'/site-parent');verify(True)
        self.site_origin=self.site_origin.replace('localhost','127.0.0.1')
        self.host.navigate(self.site_origin+'/site-parent');verify(False)
        self.toggle_and_reload('enable');verify(True)
        old=self.site_status();self.host.navigate(self.site_origin+'/site-parent')
        self.assertFalse(self.site_action('disable',generation=old['generation'])['success'])
        self.assertFalse(self.site_action('clear',origin='http://wrong.test')['success'])
        self.config(False,rules='');status=self.site_status();self.assertFalse(status['effective']);self.assertFalse(status['global_enabled'])
        self.host.navigate(self.site_origin+'/site-parent');verify(False)

    def test_scoped_reload_rejects_same_and_cross_origin_replacement(self):
        self.site_origin = self.url.rstrip('/')
        self.host.navigate(self.site_origin+'/site-parent')
        for other in [self.site_origin, self.site_origin.replace('127.0.0.1','localhost')]:
            before = self.site_status()
            original = self.site_origin
            # Replace the document, then reject the original reload scope.
            self.host.send(op='scripts', scripts=['window.reloadGuardFixture=true'])
            self.site_origin = other
            self.host.navigate(other+'/site-parent')
            current = self.site_status()
            self.assertNotEqual(current['generation'], before['generation'])
            rejected = self.site_action('reload', request=40, origin=original,
                                        generation=before['generation'])
            self.assertFalse(rejected['success'], rejected)
            self.assertEqual(rejected['generation'], before['generation'])
            self.assertEqual(self.site_status()['generation'], current['generation'])

    def test_scoped_reload_uses_updated_document_start_scripts(self):
        self.site_origin = self.url.rstrip('/')
        self.host.navigate(self.url)
        before = self.site_status()
        self.host.send(op='scripts', scripts=['window.docStartReady=true'])
        self.host.send(op='site_action', request_id=50, origin=self.site_origin,
                       generation=before['generation'], action='reload')
        result = self.host.until(lambda event: event['event']=='site_result' and event['request_id']==50)
        self.assertTrue(result['success'], result)
        self.host.until(lambda event: event['event']=='loaded' and event['url']==self.url)
        # The first inline script reads this value, so setting it in the old
        # page or after the replacement document starts cannot satisfy this.
        title = self.host.until(lambda event: event['event']=='title' and event['title'].startswith('early-'))
        self.assertEqual(title['title'], 'early-true')

    def test_burst_counts_are_aggregated_and_reset_on_document_navigation(self):
        self.config(rules='/counted-burst$xmlhttprequest')
        self.site_origin=self.url.rstrip('/')
        self.host.navigate(self.url)
        self.host.send(op='scripts',scripts=["Promise.all(Array.from({length:600},(_,i)=>fetch('/counted-burst?i='+i).catch(()=>{}))).then(()=>document.title='burst-done')"])
        self.host.until(lambda e:e['event']=='title' and e['title']=='burst-done')
        status=self.site_status();self.assertEqual(status['blocked'],600)
        self.assertEqual(sum(e['event']=='site_status' for e in self.host.seen),1,'No per-request IPC status flood')
        self.assertFalse(any('/counted-burst' in path for path in self.server.requests))
        self.host.send(op='scripts',scripts=[])
        self.host.navigate(self.url+'csp')
        reset=self.site_status(3);self.assertEqual(reset['blocked'],0);self.assertNotEqual(reset['generation'],status['generation'])

    def test_site_status_reports_actual_network_decisions(self):
        self.config()
        self.loaded()
        self.host.send(op='site_status',request_id=1,origin=self.url.rstrip('/'))
        status=self.host.until(lambda e:e['event']=='site_status' and e['request_id']==1)
        self.assertEqual(status['origin'],self.url.rstrip('/'))
        self.assertGreaterEqual(status['blocked'],2)
        self.assertEqual(status['security'],'insecure')
        self.assertTrue(status['effective'])

    def test_network_exceptions_resource_type_party_and_worker(self):
        self.config()
        self.loaded()
        self.assertNotIn('/ad.js',self.server.requests)
        self.assertNotIn('/fetch-ad',self.server.requests)
        self.assertNotIn('/worker-ad',self.server.requests)
        self.assertNotIn('/sw-ad',self.server.requests)
        self.assertNotIn('/third.js',self.server.requests)
        self.assertIn('/sw-ready',self.server.requests)
        for allowed in ['/ad.js?allow','/image-only.js','/first.js','/worker.js']:
            self.assertIn(allowed,self.server.requests)
    def test_worker_attribution_respects_frame_ownership_under_site_exception(self):
        self.config(rules='/worker-ad$xmlhttprequest\n/sw-ad$xmlhttprequest')
        self.site_origin=self.url.rstrip('/')
        self.host.send(op='site_exceptions',origins=[self.site_origin])
        self.loaded()
        self.assertIn('/worker.js',self.server.requests)
        self.assertIn('/sw-ready',self.server.requests)
        # Dedicated workers retain an owning frame here; service workers do not.
        self.assertIn('/worker-ad',self.server.requests)
        self.assertNotIn('/sw-ad',self.server.requests)
        status=self.site_status()
        self.assertTrue(status['excepted'])
        self.assertFalse(status['effective'])
        self.assertEqual(status['blocked'],0,'Unattributed workers must not charge the visible document')

    def test_subdocument_navigation_uses_initiator_context(self):
        self.config(rules='\n'.join(['/thirdparty-frame$subdocument,third-party',
            '/firstparty-frame$subdocument,~third-party',
            '/domain-frame$subdocument,domain=127.0.0.1',
            '/same-first-frame$subdocument,~third-party',
            '/same-third-frame$subdocument,third-party']))
        self.host.navigate(self.url+'iframes')
        for blocked in ['/thirdparty-frame','/domain-frame','/same-first-frame']:
            with self.subTest(blocked=blocked): self.assertNotIn(blocked,self.server.requests)
        for allowed in ['/firstparty-frame','/same-third-frame']:
            with self.subTest(allowed=allowed): self.assertIn(allowed,self.server.requests)
    def test_strict_style_csp_keeps_cosmetics_and_page_policy(self):
        self.server.csp=True
        self.config()
        self.host.navigate(self.url+'csp')
        def state():
            self.host.settle(.2)
            return json.loads(self.host.until(lambda e:e['event']=='title' and e['title'].startswith('{'))['title'])
        initial=state()
        self.assertEqual(initial['styles'],['none','none','none'])
        self.assertEqual(initial['control'],'block','Page-authored inline styles must remain blocked')
        self.assertEqual(initial['site'],'grid')
        self.config(False)
        disabled=state()
        self.assertEqual(disabled['styles'],['block','block','block'])
        self.assertEqual(disabled['control'],'block')
        self.assertEqual(disabled['site'],'grid','Disabling protection must preserve page-owned sheets')
        self.config(True)
        self.assertEqual(state()['styles'],['none','none','none'])
    def test_late_script_configuration_under_csp(self):
        self.server.csp=True
        self.host.navigate(self.url+'csp')
        script='var qosLateCount=(window.qosLateCount||0)+1'
        def state():
            self.host.settle(.2)
            return json.loads(self.host.until(lambda e:e['event']=='title' and e['title'].startswith('{'))['title'])
        self.host.send(op='scripts',scripts=[script])
        first=state()
        self.assertEqual(first['late'],1)
        self.assertFalse(first['pageEvalAllowed'],'Page-authored eval must remain blocked by CSP')
        self.host.send(op='scripts',scripts=[script])
        self.assertEqual(state()['late'],1,'Identical configuration must run once per document')
        self.host.send(op='scripts',scripts=[script+';void 0'])
        self.assertEqual(state()['late'],2)
    def test_document_start_scripts(self):
        self.host.send(op='scripts',scripts=['window.docStartReady=true'])
        self.loaded()
        self.assertEqual(self.host.latest_title(),'early-true')
        self.host.navigate(self.url.replace('127.0.0.1','localhost'))
        self.assertEqual(self.host.latest_title(),'early-true')
    def test_rapid_document_script_configuration_changes(self):
        self.host.send(op='scripts',scripts=['window.docStartReady=true'])
        self.loaded()
        self.host.send(op='scripts',scripts=['window.docStartReady=false'])
        self.host.send(op='scripts',scripts=['window.docStartReady=true'])
        self.host.navigate(self.url+'?second')
        self.assertEqual(self.host.latest_title(),'early-true')
    def test_document_start_with_content_security_policy(self):
        self.server.csp=True
        self.host.send(op='scripts',scripts=['window.docStartReady=true'])
        self.loaded()
        self.assertEqual(self.host.latest_title(),'early-true')
    def test_redirect_replacement_and_scriptlet_resources(self):
        self.config(rules=RULES+'\n/replace.js$script,redirect=noopjs\n/empty-response$xmlhttprequest,redirect=empty\n127.0.0.1##+js(set-constant, qosFixture, true)')
        self.loaded()
        self.host.send(op='scripts',scripts=["document.title=JSON.stringify({loaded:window.redirectLoaded===true,original:window.originalReplacement===true,scriptlet:window.qosFixture===true,empty:window.emptyReplacement===''})"])
        event=self.host.until(lambda e:e['event']=='title' and e['title'].startswith('{'))
        self.assertEqual(json.loads(event['title']),{'loaded':True,'original':False,'scriptlet':True,'empty':True})
        self.assertNotIn('/replace.js',self.server.requests)
    def test_initial_and_dynamic_cosmetic_selectors(self):
        self.config()
        self.loaded()
        self.assertEqual(self.styles(),['none','none','none'])
    def test_cosmetic_toggle_updates_existing_document(self):
        self.config()
        self.loaded()
        self.assertEqual(self.styles(),['none','none','none'])
        self.config(False)
        self.host.settle(.2)
        self.assertEqual(self.styles(),['block','block','block'])
        self.config(True)
        self.host.settle(.2)
        self.assertEqual(self.styles(),['none','none','none'])
    def test_disable_reenable_and_null_preserves_enabled(self):
        self.config(False)
        self.config(None)
        self.loaded()
        self.assertIn('/ad.js',self.server.requests)
        self.assertEqual(self.styles(),['block','block','block'])
        self.server.requests.clear()
        self.config(True)
        self.config(None)
        self.host.send(op='reload')
        self.host.until(lambda e:e['event']=='loaded' and e['url']==self.url)
        self.host.settle(.7)
        self.assertNotIn('/ad.js',self.server.requests)
        # A new script configuration guarantees a fresh query on this document.
        self.assertEqual(self.styles(),['none','none','none'])
    def test_internal_pages_exclude_cosmetics(self):
        self.config(rules='##body\nchrome://version##body')
        self.host.navigate('chrome://version/')
        self.assertEqual(self.host.latest_title(),'About Version')
        self.assertFalse(any(e['event']=='error' for e in self.host.seen))

if __name__=='__main__': unittest.main(verbosity=2)
