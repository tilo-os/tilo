#!/usr/bin/env python3
"""Test CEF's real startup MV3 execution and an engine-rendered action page."""
import json
import hashlib
import time
from pathlib import Path
import unittest
from capabilities import Capabilities
from native_regressions import Host
# What the fixture's content script says first, before it asks its worker.
CONTENT_RAN = 'content script ran'


def extension_page(host, url, attempts=20):
    """The fixture's report on `url`, once the extension has registered:
    an extension loads in the background, and a page opened before it is
    ready runs without its content script, so it is opened again. Once the
    content script ran, its worker's answer is waited for there: a worker
    starting for the first time takes its time, and a page left before it
    answers never hears back."""
    report = lambda e: e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'].startswith('{')
    ran = lambda e: report(e) or (e['event'] == 'title' and e.get('tab', 1) == 1 and e['title'] == CONTENT_RAN)
    for attempt in range(attempts):
        mark = len(host.seen)
        host.navigate(url + ('&' if '?' in url else '?') + 'attempt=' + str(attempt))
        if any(ran(e) for e in host.seen[mark:]):
            found = next((e for e in host.seen[mark:] if report(e)), None) or host.until(report)
            return json.loads(found['title'])
        time.sleep(.25)
    raise AssertionError('the extension never ran on ' + url + ': ' + host.latest_title())


class Extensions(Capabilities):
    def setUp(self):
        self.h=Host(args=['--load-extension='+str(Path(__file__).parent.joinpath('mv3-fixture').resolve())]);self.addCleanup(self.h.close)
    def test_mv3_worker_storage_content_and_action(self):
        probe=extension_page(self.h,self.url+'/')
        path=str(Path(__file__).parent.joinpath('mv3-fixture').resolve())
        expected=''.join(chr(ord('a')+int(n,16)) for n in hashlib.sha256(path.encode()).hexdigest()[:32])
        self.assertEqual(probe['id'],expected)
        self.assertTrue(probe['worker']);self.assertTrue(probe['content']);self.assertGreaterEqual(probe['count'],1)
        # Each visit is counted in the extension's storage.
        again=extension_page(self.h,self.url+'/?again')['count']
        self.assertEqual(again,probe['count']+1)
        self.h.navigate('chrome-extension://'+probe['id']+'/popup.html')
        self.assertEqual(self.h.latest_title(),f'extension-action-{again}')
        self.h.send(op='scripts',scripts=["document.title='injection-bug'"])
        self.h.settle()
        self.assertEqual(self.h.latest_title(),f'extension-action-{again}')
class RuntimeExtensions(Capabilities):
    """Load and unload an extension in the running engine: no restart."""
    def setUp(self):
        self.h=Host();self.addCleanup(self.h.close);self.calls=0
    def devtools(self,method,params=None,**fields):
        self.calls+=1
        self.h.send(op='devtools_call',request_id=self.calls,method=method,params=params or {},**fields)
        return self.h.until(lambda e:e['event']=='devtools_result' and e['request_id']==self.calls)
    def test_load_unload_and_denied_methods(self):
        path=str(Path(__file__).parent.joinpath('mv3-fixture').resolve())
        self.h.navigate(self.url+'/')
        self.assertFalse(self.h.latest_title().startswith('{'))
        loaded=self.devtools('Extensions.loadUnpacked',{'path':path})
        self.assertTrue(loaded['ok'],loaded)
        eid=json.loads(loaded['body'])['id']
        probe=extension_page(self.h,self.url+'/?loaded')
        self.assertEqual((probe['id'],probe['worker'],probe['content']),(eid,True,True))
        # The service worker is a target the toolbar can read.
        targets=json.loads(self.devtools('Target.getTargets')['body'])['targetInfos']
        self.assertTrue(any(t['type']=='service_worker' and t['url'].startswith('chrome-extension://'+eid+'/') for t in targets),targets)
        removed=self.devtools('Extensions.uninstall',{'id':eid})
        self.assertTrue(removed['ok'],removed)
        self.h.settle(1.0)
        self.h.navigate(self.url+'/?unloaded')
        self.assertFalse(self.h.latest_title().startswith('{'))
        self.assertFalse(self.devtools('Extensions.uninstall',{'id':eid})['ok'])
if __name__=='__main__': unittest.main(verbosity=2)
