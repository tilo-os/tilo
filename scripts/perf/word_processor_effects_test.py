import json
import unittest
from word_processor_effects import validate_typing_effects


class TypingEffectsTests(unittest.TestCase):
    def fixture(self):
        scenario = {'steps': [{'kind': 'character', 'value': 'é'},
                              {'kind': 'key', 'key': 'backspace'}]}
        def state(text, offset):
            return dict(caret_text=text, caret_offset=offset, caret_block='paragraph',
                        caret_region='body', focused=True, has_selection=False)
        a, b = state('abc', 1), state('aébc', 2)
        return scenario, [dict(id=0, before=a, after=b), dict(id=1, before=b, after=a)]

    def validate(self, scenario, effects):
        return validate_typing_effects(scenario, ['tilo-native-effect '+json.dumps(r) for r in effects])

    def test_exact_unicode_edits_pass_only_within_a_fixed_viewport(self):
        with self.subTest(phase='exact unicode edit and backspace'):
            self.assertEqual(self.validate(*self.fixture()), 2)
        with self.subTest(phase='fixed viewport rejects extra scroll even when edits are exact'):
            scenario, effects = self.fixture()
            for effect in effects:
                for state in (effect['before'], effect['after']):
                    state['offset_y'] = 0.0
            lines = lambda: ['tilo-native-effect '+json.dumps(r) for r in effects]
            self.assertEqual(validate_typing_effects(scenario, lines(), expected_offset_y=0.0), 2)
            for value in (1.0, None, True, float('nan')):
                effects[0]['before']['offset_y'] = value
                with self.subTest(value=value), self.assertRaises(ValueError):
                    validate_typing_effects(scenario, lines(), expected_offset_y=0.0)

    def test_wrong_incomplete_or_unsupported_typing_evidence_fails(self):
        with self.subTest(phase='no-op, wrong text, caret, region, selection or focus'):
            for key, value in [('caret_text', 'aebc'), ('caret_offset', 1),
                               ('caret_offset', True), ('caret_block', 'other'),
                               ('caret_region', 'header'), ('has_selection', True),
                               ('focused', False), ('caret_text', None)]:
                scenario, effects = self.fixture()
                effects = json.loads(json.dumps(effects))
                effects[0]['after'][key] = value
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    self.validate(scenario, effects)
            scenario, effects = self.fixture()
            effects[0]['after'] = effects[0]['before']
            with self.assertRaises(ValueError): self.validate(scenario, effects)
        with self.subTest(phase='missing, reordered and discontinuous evidence'):
            scenario, effects = self.fixture()
            for changed in [effects[:-1], effects[::-1], effects+effects[:1]]:
                with self.assertRaises(ValueError): self.validate(scenario, changed)
            effects = json.loads(json.dumps(effects))
            effects[1]['before']['caret_text'] = 'zébc'
            effects[1]['after']['caret_text'] = 'zbc'
            with self.assertRaises(ValueError): self.validate(scenario, effects)
        with self.subTest(phase='unsupported or empty workload'):
            scenario, effects = self.fixture()
            for step in [{'kind':'key', 'key':'delete'}, {'kind':'character','value':'ab'}]:
                scenario['steps'][0] = step
                with self.assertRaises(ValueError): self.validate(scenario, effects)
            with self.assertRaises(ValueError): self.validate({'steps':[]}, [])


if __name__ == "__main__":
    unittest.main()
