"""Local image builds date their layers by the base and keep the commit time in a label."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
COMMON = (REPO / 'os/dev/_common.sh').read_text()


def helper(name):
    body = COMMON[COMMON.index(f'{name}() {{'):]
    return body[:body.index('\n}\n') + 3]


FAKE_PODMAN = '''#!/bin/sh
# image inspect --format <template> <image>
case "$4" in
  *org.tilo.source-date-epoch*) [ -n "$LABEL" ] && echo "$LABEL" || echo "<no value>" ;;
  *Created.Unix*) [ -n "$CREATED_UNIX" ] && echo "$CREATED_UNIX" || exit 125 ;;
  *Created.UTC*) echo "$CREATED" ;;
  *) exit 99 ;;
esac
'''


class LocalImageEpoch(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        for name, text in (('podman', FAKE_PODMAN), ('skopeo', '#!/bin/sh\necho "$SKOPEO_UNIX"\n')):
            (root / name).write_text(text)
            (root / name).chmod(0o755)
        (root / 'helpers.sh').write_text(helper('tilo_image_source_time') + helper('tilo_image_epoch'))
        self.root = root

    def run_helper(self, call, **env):
        full = {'PATH': f'{self.root}:/usr/bin:/bin', 'CREATED': '2026-09-01T00:00:00Z', **env}
        return subprocess.run(['bash', '-c', f'. {self.root}/helpers.sh; {call}'], env=full,
                              capture_output=True, text=True, check=False)

    def test_base_age_comes_from_the_commit_time_label(self):
        # 1790677107 = 2026-09-29T10:18:27Z; the image itself says 2026-09-01.
        result = self.run_helper('tilo_image_source_time img', LABEL='1790677107')
        self.assertEqual(result.stdout.strip(), '2026-09-29T10:18:27Z')

    def test_base_age_falls_back_to_the_creation_time(self):
        for label in ('', 'not-a-number', '0'):
            with self.subTest(label=label):
                result = self.run_helper('tilo_image_source_time img', LABEL=label)
                self.assertEqual(result.stdout.strip(), '2026-09-01T00:00:00Z')

    def test_image_epoch_reads_local_storage_then_the_registry(self):
        self.assertEqual(self.run_helper('tilo_image_epoch img', CREATED_UNIX='1790417118').stdout.strip(),
                         '1790417118')
        self.assertEqual(self.run_helper('tilo_image_epoch img', SKOPEO_UNIX='1790417000').stdout.strip(),
                         '1790417000')
        self.assertNotEqual(self.run_helper('tilo_image_epoch img', SKOPEO_UNIX='0').returncode, 0)

    def test_build_image_uses_the_base_epoch_only_for_local_builds(self):
        script = (REPO / 'os/dev/build-image.sh').read_text()
        self.assertIn('base)\n      [ "${TILO_REQUIRE_PCR_PREDICTION:-0}" != 1 ] || die', script)
        self.assertIn('--label "org.tilo.source-date-epoch=$SOURCE_DATE_EPOCH"', script)
        self.assertIn('--build-arg "SOURCE_DATE_EPOCH=$build_epoch"', script)
        self.assertIn('--source-date-epoch "$layer_epoch" --rewrite-timestamp', script)
        local = (REPO / 'os/dev/local-inputs.sh').read_text()
        self.assertIn(': "${TILO_LAYER_EPOCH:=base}"', local)
        dev = (REPO / 'os/dev/build-dev-image.sh').read_text()
        # A caller that knows the base's commit time (t's os-dev-image) passes it;
        # otherwise it is read from the local base.
        self.assertIn('base_created="${TILO_DEV_BASE_CREATED:-$(tilo_image_source_time "$base")}"', dev)


if __name__ == '__main__':
    unittest.main()
