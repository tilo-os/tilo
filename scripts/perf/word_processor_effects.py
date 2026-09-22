"""Exact body-paragraph edit assertions for controlled native WP fixtures."""
from native_delivery import rows


def validate_typing_effects(scenario, lines, *, expected_offset_y=None):
    effects = rows(lines, 'qos-native-effect ')
    steps = scenario['steps']
    if len(effects) != len(steps):
        raise ValueError('Missing or extra Word Processor effect evidence')
    fields = ('caret_text', 'caret_offset', 'caret_block', 'caret_region',
              'focused', 'has_selection')
    previous = None
    edits = 0
    for index, (step, effect) in enumerate(zip(steps, effects)):
        if type(effect.get('id')) is not int or effect['id'] != index:
            raise ValueError('Reordered Word Processor effect identity')
        if step['kind'] in ('hover', 'click'):
            previous = None
            continue
        before, after = effect.get('before'), effect.get('after')
        for state in (before, after):
            if expected_offset_y is not None and (
                    not isinstance(state, dict)
                    or type(state.get('offset_y')) not in (int, float)
                    or state['offset_y'] != expected_offset_y):
                raise ValueError('Unexpected viewport movement during fixed-view typing')
            if (not isinstance(state, dict)
                    or type(state.get('caret_text')) is not str
                    or type(state.get('caret_offset')) is not int
                    or not 0 <= state['caret_offset'] <= len(state['caret_text'])
                    or type(state.get('caret_block')) is not str or not state['caret_block']
                    or state.get('caret_region') != 'body'
                    or state.get('focused') is not True
                    or state.get('has_selection') is not False):
                raise ValueError('Invalid focused body caret snapshot')
        if previous is not None and any(previous[k] != before[k] for k in fields):
            raise ValueError('Discontinuous editing state between inputs')
        text, offset = before['caret_text'], before['caret_offset']
        if step['kind'] == 'character':
            char = step.get('value')
            if type(char) is not str or len(char) != 1 or ord(char) < 32:
                raise ValueError('Expected a single printable character')
            expected_text, expected_offset = text[:offset]+char+text[offset:], offset+1
        elif step['kind'] == 'key' and step.get('key') == 'backspace' and offset > 0:
            expected_text, expected_offset = text[:offset-1]+text[offset:], offset-1
        else:
            raise ValueError('Unsupported typing step or cross-block backspace')
        if (after['caret_block'] != before['caret_block']
                or after['caret_text'] != expected_text
                or after['caret_offset'] != expected_offset):
            raise ValueError('Requested edit did not produce exact text/caret transition')
        previous = after
        edits += 1
    if not edits:
        raise ValueError('No editing steps were validated')
    return edits
