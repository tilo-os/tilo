"""Fail-closed public binding and receipt decoding guards."""
import importlib.util
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from acceptance.vm import TiloVm, VmError, first_json

spec = importlib.util.spec_from_file_location('terminal_cohort', Path(__file__).with_name('terminal-cohort.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class TerminalCohort(unittest.TestCase):
    def row(self, resource='bound', operation='run'):
        return dict(application=module.APP, room='room', record='record', resource=resource,
                    provider='app-actions', operation=operation,
                    semantic_identity={'kind': 'terminal.session', 'key': 'terminal.session'},
                    contract={'availability': {'state': 'available'}})

    def test_scoped_machine_catalogue_not_human_suggestions(self):
        expected = self.row(operation="inspect-session")
        class Vm(TiloVm):
            def control_json(self, *args):
                self.args = args
                return {"actions": [expected] if "--programmatic" in args else []}
        vm = Vm("tilo-vm", "test-vm", "test-slot", Path("."))
        self.assertEqual(vm.actions(module.APP), [])
        self.assertEqual(module.terminal_actions(vm), [expected])
        self.assertEqual(vm.args, ("actions", "list", module.APP, "--programmatic", "--json", "--limit", "2000"))
        self.assertIs(module.select(module.terminal_actions(vm), "inspect-session"), expected)

    def test_same_bound_connection_only(self):
        row = self.row()
        self.assertIs(module.select([row, self.row('other')], 'run', module.identity(row)), row)
        with self.assertRaises(VmError):
            module.select([self.row('other')], 'run', module.identity(row))

    def test_ambiguous_missing_denied_are_errors(self):
        row = self.row()
        for rows in ([], [row, row]):
            with self.assertRaises(VmError):
                module.select(rows, 'run')
        row['contract']['availability']['state'] = 'unavailable'
        with self.assertRaises(VmError):
            module.select([row], 'run')

    def test_title_does_not_imply_authority(self):
        row = self.row()
        row['semantic_identity'] = None
        row['title'] = 'Run command'
        with self.assertRaises(VmError):
            module.select([row], 'run')

    def test_acceptance_is_not_completion(self):
        with self.assertRaises(VmError):
            module.succeeded({'receipt': {'update': {'state': 'accepted'}}})

    def test_pretty_and_compact_canonical_json(self):
        self.assertEqual(first_json('{\n "slot": {}\n}\n'), {'slot': {}})
        self.assertEqual(first_json('warning\n{"slot": {}}\n'), {'slot': {}})
        with self.assertRaises(VmError):
            first_json('unobservable')


if __name__ == '__main__':
    unittest.main()
