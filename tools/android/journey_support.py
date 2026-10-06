"""Drive the existing external accessibility surface of an owned fixture bus."""
import re
import subprocess
import sys
from run_arity_journey import REPO, wait_for
sys.path.insert(0, str(REPO / 'tools'))
from atspi_external_client import AtspiClient, GateError, bus_pids


class AccessiblePicker:
    def __init__(self, bus_address):
        def call(method, *arguments):
            return subprocess.run(['gdbus', 'call', '--address', bus_address,
                '--dest', 'org.a11y.Bus', '--object-path', '/org/a11y/bus',
                '--method', method, *arguments], text=True, capture_output=True,
                check=True, timeout=10).stdout
        for property_name in ('IsEnabled', 'ScreenReaderEnabled'):
            call('org.freedesktop.DBus.Properties.Set', 'org.a11y.Status', property_name, '<true>')
        match = re.search(r"'([^']+)'", call('org.a11y.Bus.GetAddress'))
        if not match: raise RuntimeError('owned session has no AT-SPI address')
        self.address = match.group(1)

    def described(self, expected, limit=1024):
        for bus_name in bus_pids(self.address):
            client = AtspiClient(self.address, bus_name)
            try:
                tree = client.tree(limit)
                paths = client.named(tree, expected)
                if len(paths) == 1: return client, paths[0], tree
            except GateError:
                continue
        return None

    def activate(self, expected, before_click=None):
        client, path, tree = wait_for(lambda: self.described(expected),
            'external AT-SPI picker control ' + expected)
        if before_click: before_click()
        client.click(tree, expected)
        return {'bus_name': client.bus_name, 'path': path, 'tree': tree}
