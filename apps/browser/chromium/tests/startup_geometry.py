#!/usr/bin/env python3
"""Verify startup layout commands reach a current physical paint before input."""
import queue
import time
import unittest

from native_regressions import Host


class StartupGeometry(unittest.TestCase):
    def test_immediate_private_layout_reaches_retina_paint_without_input(self):
        for delay in (.001, .003, .005, .01, .02, .04):
            with self.subTest(first_resize_delay=delay):
                try:
                    self._assert_startup_resize(delay)
                finally:
                    self.doCleanups()

    def _assert_startup_resize(self, delay):
        host = Host(('--private',))
        self.addCleanup(host.close)
        # Start with an allocated CEF surface, as the live app does while
        # constructing its first layout. Do not await intermediate paints.
        self.assertEqual(host.frames.get(timeout=10), (1280, 800))
        host.send(op='navigate', url='data:text/html,<title>resize</title><body>resize</body>')
        host.send(op='resize', width=800, height=452, scale_factor=1)
        # Let the first resize enter CEF before later layout passes arrive,
        # without waiting for its asynchronous paint/resize acknowledgement.
        time.sleep(delay)
        for width, height, scale in [(800, 444, 1), (800, 444, 1),
                                      (1600, 888, 2), (1600, 888, 2)]:
            host.send(op='resize', width=width, height=height, scale_factor=scale)
        time.sleep(.01)
        for _ in range(3):
            host.send(op='resize', width=1600, height=904, scale_factor=2)
        deadline = time.monotonic() + 10
        seen = []
        try:
            while True:
                size = host.frames.get(timeout=max(.01, deadline-time.monotonic()))
                seen.append(size)
                if size == (1600, 904):
                    break
                self.assertLess(time.monotonic(), deadline, seen)
        except queue.Empty:
            self.fail(f'No current physical startup paint; received {seen}')
        finally:
            host.log.seek(0)
            print(host.log.read().decode(errors='replace'))

    def test_hidden_resize_reaches_latest_geometry_when_shown(self):
        host = Host(('--private',))
        self.addCleanup(host.close)
        self.assertEqual(host.frames.get(timeout=10), (1280, 800))
        host.send(op='visibility', visible=False)
        host.send(op='resize', width=800, height=452, scale_factor=1)
        host.send(op='resize', width=1600, height=904, scale_factor=2)
        # This response establishes native dispatch without awaiting a paint
        # from a hidden view (CEF intentionally suspends those paints).
        host.send(op='audio_muted', muted=False)
        host.until(lambda e: e['event'] == 'audio_muted')
        host.send(op='visibility', visible=True)
        deadline = time.monotonic() + 10
        seen = []
        try:
            while True:
                size = host.frames.get(timeout=max(.01, deadline-time.monotonic()))
                seen.append(size)
                if size == (1600, 904):
                    break
                self.assertLess(time.monotonic(), deadline, seen)
        except queue.Empty:
            self.fail(f'Hidden resize did not resume when shown; received {seen}')

    def test_close_does_not_wait_for_hidden_resize_paint(self):
        host = Host(('--private',))
        self.addCleanup(host.close)
        host.send(op='visibility', visible=False)
        host.send(op='resize', width=1600, height=904, scale_factor=2)
        host.send(op='audio_muted', muted=False)
        host.until(lambda e: e['event'] == 'audio_muted')
        # Cleanup sends shutdown and requires the helper to exit. It must not
        # depend on an acknowledgement paint that the hidden view cannot emit.


if __name__ == '__main__':
    unittest.main(verbosity=2)
