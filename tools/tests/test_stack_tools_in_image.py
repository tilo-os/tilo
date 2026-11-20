"""A stack on a build server finds the tools it asks for in the image it starts in.

A stack's build container has no network, so the programs the stack starts for
itself (openssl for the releases it signs, Caddy for its TLS) must be in the CI
builder image it may start in, at the versions this tree pins. This holds the
image to the pin, and the pin to both architectures' archives.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
BUILDER = ROOT / 'os/Containerfile.builder'
CADDY = ROOT / 'os/containers/tilo-sh/caddy.env'


def pins():
    text = CADDY.read_text()
    return dict(re.findall(r'^(CADDY_[A-Z0-9_]+)=(\S+)$', text, re.M))


class StackToolsInImageTests(unittest.TestCase):
    def test_the_image_installs_openssl_and_caddy(self):
        text = BUILDER.read_text()
        self.assertIn('openssl', text)
        self.assertIn('/usr/local/bin/caddy', text)

    def test_the_image_takes_caddys_version_from_the_stack_pin(self):
        text = BUILDER.read_text()
        self.assertIn('os/containers/tilo-sh/caddy.env', text)
        self.assertIsNone(
            re.search(r'caddy_[0-9]+\.[0-9]+\.[0-9]+_', text),
            'the image names a Caddy version instead of reading the stack pin')

    def test_both_architectures_archives_are_pinned(self):
        pinned = pins()
        self.assertRegex(pinned.get('CADDY_VERSION', ''), r'^[0-9]+\.[0-9]+\.[0-9]+$')
        for key in ('CADDY_SHA512', 'CADDY_SHA512_ARM64'):
            self.assertRegex(pinned.get(key, ''), r'^[0-9a-f]{128}$', key)


if __name__ == '__main__':
    unittest.main()
