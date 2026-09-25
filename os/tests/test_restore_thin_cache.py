import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from contextlib import contextmanager

from frozen_source_fixture import NATIVE_RECIPE

ROOT = Path(__file__).resolve().parents[2]
def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'os/dev' / file)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module); return module
m = load('restore_thin', 'restore-thin-cache.py')
resume = load('native_resume', 'native_resume.py')
stage = load('thin_stage', 'thin_cache_stage.py')
launcher = load('thin_launcher', 'launch-native-resume.py')
compiler = load('thin_compile_controller', 'compile-development-os.py')


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
                      producer_helper_revision=m.HELPER,checkpoint_reference='code.podesta.ai/qos/tilo-development-partial-thin-cache-aarch64@sha256:'+'a'*64,
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

    def test_frozen_recipe_seam_preserves_cargo_graph_and_tail(self):
        original=NATIVE_RECIPE
        normal,_=resume.derive_recipe(original,{'browser'},['tilo'])
        derived,_=resume.derive_recipe(original,{'browser'},['tilo'],thin_cache_approval_sha256='a'*64)
        self.assertLess(derived.index('cargo clean --profile thin-app'),derived.index('python3 /tilo-thin-cache/restore-thin-cache.py'))
        self.assertLess(derived.index('cp /tilo-resume/partial/*'),derived.index('python3 /tilo-thin-cache/restore-thin-cache.py'))
        marker='    && build_phase rust-output-validation'
        export = 'COPY --from=builder /build/thin-cache-restoration.json /out/thin-cache-restoration.json\n'
        self.assertEqual(derived.count(export), 1)
        self.assertEqual(normal[normal.index(marker):],derived[derived.index(marker):].replace(export, ''))
        payload = derived.split('FROM scratch AS native-builder-payload\n', 1)[1].split('\nFROM ', 1)[0]
        self.assertIn(export, payload)
        self.assertEqual(normal.count('/build/os/scripts/build-thin-apps.sh'),1)
        self.assertIn('from=tilo-thin-cache,target=/tilo-thin-cache,ro',derived)
        with self.assertRaises(ValueError):resume.derive_recipe(original,{'browser'},['tilo'],thin_cache_approval_sha256='bad')

    def test_public_evidence_survives_scratch_projection_and_is_validated(self):
        import shutil
        with tempfile.TemporaryDirectory() as d:
            base = Path(d); builder = base / 'builder'; payload = base / 'payload'
            (builder / 'build').mkdir(parents=True); (payload / 'out').mkdir(parents=True)
            receipt = dict(purpose='tilo-incomplete-thin-cache-restored', approval_sha256='a'*64,
                           cargo_revalidation_required=True, complete=False)
            raw = json.dumps(receipt).encode()
            (builder / 'build/thin-cache-restoration.json').write_bytes(raw)
            # Reproduce the exact optional COPY across the final scratch boundary.
            shutil.copy2(builder / 'build/thin-cache-restoration.json',
                         payload / resume.THIN_CACHE_EVIDENCE)
            self.assertFalse((payload / 'build').exists())
            def bounded(path, limit):
                data = path.read_bytes(); self.assertLessEqual(len(data), limit); return data
            tools = SimpleNamespace(read_bounded=bounded, strict_json=json.loads)
            self.assertEqual(resume.read_thin_cache_evidence(tools, payload, 'a'*64), (raw, receipt))
            with self.assertRaisesRegex(ValueError, 'actual thin cache'):
                resume.read_thin_cache_evidence(tools, payload, 'b'*64)
            (payload / resume.THIN_CACHE_EVIDENCE).unlink()
            with self.assertRaises(FileNotFoundError):
                resume.read_thin_cache_evidence(tools, payload, 'a'*64)

    def test_registry_stage_verifies_public_bytes_and_preserves_approval(self):
        with tempfile.TemporaryDirectory() as d:
            base=Path(d);context,root,digest,stamp=self.fixture(base)
            approval=json.loads((context/'approval.json').read_bytes())
            approval.update(checkpoint_config_digest='sha256:'+'b'*64,
                source_metadata_reference='code.podesta.ai/qos/tilo-development-thin-cache-metadata-aarch64@sha256:'+'c'*64,
                source_metadata_config_digest='sha256:'+'d'*64)
            (context/'approval.json').write_text(json.dumps(approval));raw=(context/'approval.json').read_bytes()
            image=base/'image';image.mkdir();(image/'checkpoint').symlink_to(context,target_is_directory=True)
            # A real mounted image contains a regular directory, not a symlink.
            (image/'checkpoint').unlink();import shutil;shutil.copytree(context,image/'checkpoint')
            @contextmanager
            def mounted(identity):yield image
            identities={approval['checkpoint_reference']:approval['checkpoint_config_digest'],
                        approval['source_metadata_reference']:approval['source_metadata_config_digest']}
            tools=SimpleNamespace(read_bounded=lambda p,n:p.read_bytes(),sha=lambda b:hashlib.sha256(b).hexdigest(),
                strict_json=json.loads,registry_identity=lambda ref,arch:identities[ref],command=lambda *a:None,
                inspect_local=lambda ref,arch,config:ref,mounted=mounted)
            work=base/'work';work.mkdir()
            result,provenance=stage.stage(SimpleNamespace(tools=tools),context/'approval.json',hashlib.sha256(raw).hexdigest(),work)
            self.assertEqual((result/'approval.json').read_bytes(),raw)
            self.assertEqual(m.sha(result/'thin-cache.tar'),approval['archive_sha256'])
            self.assertFalse(provenance['complete'])
            (image/'checkpoint/thin-cache.tar').write_bytes(b'tampered')
            work2=base/'work2';work2.mkdir()
            with self.assertRaisesRegex(ValueError,'public file'):stage.stage(SimpleNamespace(tools=tools),context/'approval.json',hashlib.sha256(raw).hexdigest(),work2)

    def test_launcher_and_controller_forward_exact_optional_approval(self):
        hashes={'WPE_ADOPTION_SHA256':'a'*64,'CHECKPOINT_INITIAL_SHA256':'b'*64,'CHECKPOINT_DELTA_SHA256':'c'*64}
        normal=launcher.driver_argv('d'*40,hashes)
        hashes['THIN_CACHE_APPROVAL_SHA256']='e'*64
        extended=launcher.driver_argv('d'*40,hashes)
        self.assertEqual(extended[:-3],normal)
        self.assertEqual(extended[-3:],['--resume-thin-cache-approval','/work/thin-cache-approval.json','e'*64])
        hashes[launcher.ADMISSION+'_SHA256']='f'*64
        for label in launcher.EXTRA_PARTIALS:hashes[label+'_SHA256']='a'*64
        combined=launcher.driver_argv('d'*40,hashes)
        self.assertEqual(combined.count('--resume-checkpoint'),4)
        self.assertIn('--resume-thin-cache-approval',combined)
        self.assertIn('--resume-reconstructed-partial-admission',combined)
        with tempfile.TemporaryDirectory() as d:
            args=SimpleNamespace(resume_checkpoint=[('checkpoint.json','b'*64)],
                resume_thin_cache_approval=['/work/thin-cache-approval.json','e'*64],
                resume_reconstructed_partial_admission=['/work/reconstructed-partial-admission.json','f'*64])
            commands=[]
            with patch.object(compiler,'PRIVATE',Path(d)),patch.object(compiler,'run',lambda *a,**kw:commands.append(a)),patch.dict(os.environ,{'GITHUB_RUN_ID':'1','GITHUB_RUN_ATTEMPT':'1'}):
                compiler.resume_selected_native(args,{'fixture':True},{'fixture':'environment'})
            index=commands[0].index('--thin-cache-approval')
            self.assertEqual(list(commands[0][index:index+3]),['--thin-cache-approval','/work/thin-cache-approval.json','e'*64])
            self.assertEqual(list(commands[0][-3:]),['--reconstructed-partial-admission','/work/reconstructed-partial-admission.json','f'*64])


if __name__=='__main__':unittest.main()
