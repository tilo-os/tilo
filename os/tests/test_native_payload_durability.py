import hashlib
import importlib.util
import json
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('durable_native', ROOT / 'os/dev/native_resume.py')
resume = importlib.util.module_from_spec(spec); spec.loader.exec_module(resume)


class Durability(unittest.TestCase):
    def fixture(self, base, failure=None, mismatch=False):
        root=base/'root';root.mkdir();work=base/'work';work.mkdir();evidence=base/'evidence';evidence.mkdir()
        events=[];inventory={'out/fixture': {'type':'file','size':1,'sha256':'e'*64}}
        @contextmanager
        def mounted(identity):
            events.append('mount')
            try: yield root
            finally: events.append('unmount')
        def audit(root, inputs):
            events.append('audit')
            if failure: raise failure
            return inventory
        def command(*args):
            events.append('push');Path(args[args.index('--digestfile')+1]).write_text('sha256:'+'b'*64)
            self.assertIn('tilo-development-unapproved-native-payload-aarch64:candidate-21842-1',args[-1])
        tools=SimpleNamespace(inspect_local=lambda *a:events.append('inspect'),mounted=mounted,
            canonical=lambda value:json.dumps(value,sort_keys=True,separators=(',',':')).encode(),
            sha=lambda raw:hashlib.sha256(raw).hexdigest(),command=command,
            registry_identity=lambda *a:'sha256:'+('c' if mismatch else 'a')*64)
        native=SimpleNamespace(tools=tools,METADATA='out/tilo-native-builder-artifact.json',inventory=audit)
        return native,root,work,evidence,events,inventory

    def test_failures_never_publish_or_lose_checkpoint(self):
        with self.subTest(phase="late_validation_failure_preserves_checkpoint_and_original_exception"):
            with tempfile.TemporaryDirectory() as d:
                native,root,work,evidence,events,inventory=self.fixture(Path(d))
                reconstruction={'producer_sha256':'d'*64};failure=FileNotFoundError('missing runtime receipt')
                with self.assertRaises(FileNotFoundError) as caught:
                    with resume.checkpointed_payload(native,'a'*64,{'architecture':'aarch64'},reconstruction,work,evidence,21842,1) as (actual,files):
                        self.assertEqual((actual,files),(root,inventory))
                        self.assertTrue((evidence/'unapproved-payload.json').exists())
                        reconstruction['runtime_evidence']='later mutation'
                        (evidence/'reconstruction.json').write_text(json.dumps(reconstruction))
                        raise failure
                self.assertIs(caught.exception,failure)
                receipt=json.loads((evidence/'unapproved-payload.json').read_bytes())
                snapshot=(evidence/'unapproved-reconstruction.json').read_bytes()
                self.assertEqual(receipt['reconstruction_evidence_sha256'],hashlib.sha256(snapshot).hexdigest())
                self.assertNotIn('runtime_evidence',json.loads(snapshot))
                # Launcher reserializes bounded public JSON; digest is explicitly canonical.
                exported = json.dumps(json.loads(snapshot))+'\n'
                self.assertEqual(receipt['reconstruction_evidence_digest_format'], 'json-sort-keys-compact-no-newline')
                self.assertEqual(receipt['reconstruction_evidence_sha256'], native.tools.sha(native.tools.canonical(json.loads(exported))))
                self.assertFalse(receipt['complete_native_builder']);self.assertFalse(receipt['installable'])
                self.assertFalse(receipt['producer_validation_complete']);self.assertTrue(receipt['consumer_adoption_required'])
                self.assertEqual(events,['inspect','mount','audit','push','unmount'])
        with self.subTest(phase="invalid_inventory_or_unchecked_metadata_never_publishes"):
            for kind in ('inventory','metadata'):
                with self.subTest(kind=kind),tempfile.TemporaryDirectory() as d:
                    native,root,work,evidence,events,_=self.fixture(Path(d),ValueError('private material') if kind=='inventory' else None)
                    if kind=='metadata':
                        (root/'out').mkdir();(root/native.METADATA).write_text('unchecked secret')
                    with self.assertRaises(ValueError):
                        with resume.checkpointed_payload(native,'a'*64,{'architecture':'aarch64'},{},work,evidence,21842,1):
                            self.fail('invalid payload admitted')
                    self.assertNotIn('push',events);self.assertFalse((evidence/'unapproved-payload.json').exists())
        with self.subTest(phase="wrong_registry_identity_never_writes_verified_receipt"):
            with tempfile.TemporaryDirectory() as d:
                native,root,work,evidence,events,_=self.fixture(Path(d),mismatch=True)
                with self.assertRaisesRegex(ValueError,'published unapproved payload differs'):
                    with resume.checkpointed_payload(native,'a'*64,{'architecture':'aarch64'},{},work,evidence,21842,1):
                        self.fail('wrong registry config accepted')
                self.assertFalse((evidence/'unapproved-payload.json').exists())


if __name__=='__main__':unittest.main()
