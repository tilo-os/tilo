import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'os/dev' / file)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module); return module
m = load('restore_thin', 'restore-thin-cache.py')
resume = load('native_resume', 'native_resume.py')


class Cache(unittest.TestCase):
    def fixture(self, base):
        context=base/'context';context.mkdir();root=base/'build';root.mkdir();(root/'target').mkdir()
        source=root/'Cargo.toml';source.write_bytes(b'workspace');os.chmod(source,0o644)
        stamp=1700000000123456789
        data=b'complete cached bytes';digest=hashlib.sha256(data).hexdigest()
        archive=context/'thin-cache.tar'
        with tarfile.open(archive,'w',format=tarfile.PAX_FORMAT) as tar:
            info=tarfile.TarInfo('deps/unit.rlib');info.mode=0o644;info.size=len(data)
            tar.addfile(info,io.BytesIO(data))
        metadata=dict(purpose='tilo-development-incomplete-thin-cargo-cache',source_revision=m.SOURCE,
                      helper_revision=m.HELPER,source_path='/build/target/thin-app',installable=False,
                      resume_admitted=False,complete=False,cargo_revalidation_required=True,
                      archive_bytes=archive.stat().st_size,archive_sha256=m.sha(archive),
                      inventory={'deps/unit.rlib':dict(size=len(data),sha256=digest,mode=0o644,mtime_ns=stamp)})
        sources=dict(purpose='incomplete-thin-cargo-source-metadata',source_revision=m.SOURCE,build_root='/build',
                     tracked_files=[dict(path='Cargo.toml',size=9,sha256=m.sha(source),mode=0o644,mtime_ns=stamp)],skipped=[],rpm_nevras_sha256='a'*64)
        for name,value in [('cache-metadata.json',metadata),('source-metadata.json',sources)]:
            (context/name).write_text(json.dumps(value))
        approval=dict(purpose='tilo-incomplete-thin-cache-admission',source_revision=m.SOURCE,
                      producer_helper_revision=m.HELPER,checkpoint_reference='code.sandbox.example/qos/tilo-development-partial-thin-cache-aarch64@sha256:'+'a'*64,
                      restore_helper_sha256=m.sha(Path(m.__file__)),cargo_revalidation_required=True,incomplete_cache_reviewed=True,
                      metadata_sha256=m.sha(context/'cache-metadata.json'),source_metadata_sha256=m.sha(context/'source-metadata.json'),
                      archive_sha256=m.sha(archive))
        (context/'approval.json').write_text(json.dumps(approval))
        return context,root,m.sha(context/'approval.json'),stamp

    def test_exact_cache_and_source_timestamps_without_freshness_claim(self):
        with tempfile.TemporaryDirectory() as d:
            context,root,approval,stamp=self.fixture(Path(d))
            with patch.object(m,'validate_toolchain'):m.restore(context,approval,root)
            self.assertEqual((root/'target/thin-app/deps/unit.rlib').stat().st_mtime_ns,stamp)
            self.assertEqual((root/'Cargo.toml').stat().st_mtime_ns,stamp)
            result=json.loads((root/'thin-cache-restoration.json').read_bytes())
            self.assertTrue(result['cargo_revalidation_required']);self.assertFalse(result['cargo_freshness_proven'])
            self.assertFalse(result['dependency_timestamps_restored']);self.assertEqual(result['restored_files'],1)

    def test_source_mismatch_and_nonempty_destination_leave_source_timestamp(self):
        for change in ('source','destination','approval','toolchain'):
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                context,root,approval,stamp=self.fixture(Path(d))
                source=root/'Cargo.toml'
                if change=='source':source.write_bytes(b'changedxx')
                if change=='destination':
                    dest=root/'target/thin-app';dest.mkdir();(dest/'active').write_text('active')
                if change=='approval':approval='0'*64
                before=source.stat().st_mtime_ns
                with patch.object(m,'validate_toolchain',side_effect=ValueError('toolchain') if change=='toolchain' else None):
                    with self.assertRaises(ValueError):m.restore(context,approval,root)
                self.assertEqual(source.stat().st_mtime_ns,before)
                self.assertFalse((root/'thin-cache-restoration.json').exists())

    def test_tar_rejects_traversal_links_duplicates_wrong_hash(self):
        for kind in ('traversal','link','duplicate','hash'):
            with self.subTest(kind=kind),tempfile.TemporaryDirectory() as d:
                root=Path(d);archive=root/'bad.tar';out=root/'out';out.mkdir()
                name='../escape' if kind=='traversal' else 'deps/unit'
                with tarfile.open(archive,'w') as tar:
                    for _ in range(2 if kind=='duplicate' else 1):
                        info=tarfile.TarInfo(name);info.mode=0o644;info.size=1
                        if kind=='link':info.type=tarfile.SYMTYPE;info.linkname='/outside';info.size=0
                        tar.addfile(info,io.BytesIO(b'x'))
                metadata={'inventory':{name:dict(size=1,sha256='0'*64 if kind=='hash' else hashlib.sha256(b'x').hexdigest(),mode=0o644,mtime_ns=1)}}
                with self.assertRaises(ValueError):m.unpack(archive,metadata,out)
                self.assertFalse((root/'escape').exists())

    def test_inner_executable_bytes_are_checked(self):
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'tool';path.write_bytes(b'exact')
            records=[dict(command=name,path=str(path),resolved_namespace_path=str(path),size=5,sha256=m.sha(path),version=['fixture']) for name in ('cargo','rustc','cc','ld')]
            metadata={'inner_tools':records,'rpm_nevras':['fixture'],'rpm_nevras_sha256':hashlib.sha256(b'fixture\n').hexdigest()}
            m.validate_toolchain(metadata,lambda args:b'fixture\n')
            path.write_bytes(b'other')
            with self.assertRaisesRegex(ValueError,'tool bytes'):m.validate_toolchain(metadata,lambda args:b'fixture\n')
            with self.assertRaisesRegex(ValueError,'installed RPM'):m.validate_toolchain(metadata,lambda args:b'different\n')

    def test_real_frozen_recipe_seam_preserves_cargo_graph_and_tail(self):
        original=subprocess.check_output(['git','-C',str(ROOT),'show',m.SOURCE+':os/Containerfile']).decode()
        normal,_=resume.derive_recipe(original,{'browser'},['tilo'])
        derived,_=resume.derive_recipe(original,{'browser'},['tilo'],thin_cache_approval_sha256='a'*64)
        self.assertLess(derived.index('cargo clean --profile thin-app'),derived.index('python3 /tilo-thin-cache/restore-thin-cache.py'))
        self.assertLess(derived.index('cp /tilo-resume/partial/*'),derived.index('python3 /tilo-thin-cache/restore-thin-cache.py'))
        marker='    && build_phase rust-output-validation'
        self.assertEqual(normal[normal.index(marker):],derived[derived.index(marker):])
        self.assertEqual(normal.count('/build/os/scripts/build-thin-apps.sh'),1)
        self.assertIn('from=tilo-thin-cache,target=/tilo-thin-cache,ro',derived)
        with self.assertRaises(ValueError):resume.derive_recipe(original,{'browser'},['tilo'],thin_cache_approval_sha256='bad')


if __name__=='__main__':unittest.main()
