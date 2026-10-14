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
if __name__=='__main__': unittest.main(verbosity=2)
