#!/usr/bin/env python3
"""Test CEF's real startup MV3 execution and an engine-rendered action page."""
import json
import hashlib
from pathlib import Path
import unittest
from capabilities import Capabilities
from native_regressions import Host
class Extensions(Capabilities):
    def setUp(self):
        self.h=Host(args=['--load-extension='+str(Path(__file__).parent.joinpath('mv3-fixture').resolve())]);self.addCleanup(self.h.close)
    def test_mv3_worker_storage_content_and_action(self):
        self.h.navigate(self.url+'/')
        title=self.h.latest_title()
        self.assertTrue(title.startswith('{'), title)
        probe=json.loads(title)
        path=str(Path(__file__).parent.joinpath('mv3-fixture').resolve())
        expected=''.join(chr(ord('a')+int(n,16)) for n in hashlib.sha256(path.encode()).hexdigest()[:32])
        self.assertEqual(probe['id'],expected)
        self.assertTrue(probe['worker']);self.assertTrue(probe['content']);self.assertEqual(probe['count'],1)
        self.h.navigate(self.url+'/?again')
        self.assertEqual(json.loads(self.h.latest_title())['count'],2)
        self.h.navigate('chrome-extension://'+probe['id']+'/popup.html')
        self.assertEqual(self.h.latest_title(),'extension-action-2')
        self.h.send(op='scripts',scripts=["document.title='injection-bug'"])
        self.h.settle()
        self.assertEqual(self.h.latest_title(),'extension-action-2')
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
        self.h.settle(1.5)
        self.h.navigate(self.url+'/?loaded')
        probe=json.loads(self.h.latest_title())
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
