"""Validate Code Editor model/widget effects, never displayed-frame completion.

Each validator returns the number of verified edits, wheel movements, or frames.
Native timing/cadence and CPU/worker correlation remain separate validations.
"""
import json
import math
import re


_STATE_KEYS = (
    'app', 'editor_id', 'hash_algorithm', 'buffer_id', 'ready', 'body_hash',
    'body_bytes', 'editor_focused', 'caret_line', 'caret_column', 'caret_count',
    'selection_chars', 'offset_x', 'offset_y', 'offset_x_unit', 'offset_y_unit',
    'root_rect', 'editor_rect',
)


def _rows(lines, prefix):
    result = []
    for line in lines:
        if not isinstance(line, str):
            raise ValueError('Invalid native log line')
        if line.startswith(prefix):
            row = json.loads(line[len(prefix):])
            if not isinstance(row, dict):
                raise ValueError('Native evidence must be an object')
            result.append(row)
    return result


def _number(value):
    return type(value) in (int, float) and math.isfinite(value)


def _hash(value):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) is not None


def _manifest(value):
    if (not isinstance(value, dict) or type(value.get('schema_version')) is not int
            or value['schema_version'] != 1 or value.get('hash_algorithm') != 'sha256'
            or value.get('editor_id') != 'editor_view'
            or type(value.get('lines')) is not int or value['lines'] <= 0):
        raise ValueError('Invalid Code Editor fixture manifest')
    for key in ('body_hash', 'prefixed_x_hash'):
        if not _hash(value.get(key)):
            raise ValueError('Invalid fixture SHA256')
    for key in ('body_bytes', 'prefixed_x_bytes'):
        if type(value.get(key)) is not int or value[key] <= 0:
            raise ValueError('Invalid fixture byte count')
    if (value['prefixed_x_bytes'] != value['body_bytes'] + 1
            or value['prefixed_x_hash'] == value['body_hash']):
        raise ValueError('Invalid fixture edit transition')
    return ((value['body_hash'], value['body_bytes']),
            (value['prefixed_x_hash'], value['prefixed_x_bytes']))


def _state(value):
    if (not isinstance(value, dict) or any(key not in value for key in _STATE_KEYS)
            or value.get('ready') is not True or value.get('app') != 'dev.qos.code-editor'
            or value.get('editor_id') != 'editor_view'
            or value.get('hash_algorithm') != 'sha256'
            or not isinstance(value.get('buffer_id'), str) or not value['buffer_id'].strip()
            or not _hash(value.get('body_hash'))
            or type(value.get('body_bytes')) is not int or value['body_bytes'] < 0
            or type(value.get('editor_focused')) is not bool):
        raise ValueError('Malformed or unready Code Editor state')
    for key in ('caret_line', 'caret_column', 'caret_count', 'selection_chars'):
        if type(value.get(key)) is not int or value[key] < 0:
            raise ValueError('Invalid native caret/selection state')
    if value['caret_count'] != 1 or value['selection_chars'] != 0:
        raise ValueError('Workload requires one caret and no selection')
    if value['offset_x_unit'] != 'pixels' or value['offset_y_unit'] != 'lines':
        raise ValueError('Incorrect Code Editor scroll units')
    for key in ('offset_x', 'offset_y'):
        if not _number(value[key]) or value[key] < 0:
            raise ValueError('Invalid native scroll offset')
    for key in ('root_rect', 'editor_rect'):
        rect = value[key]
        if (not isinstance(rect, list) or len(rect) != 4
                or not all(_number(v) for v in rect)
                or rect[2] <= rect[0] or rect[3] <= rect[1]):
            raise ValueError('Invalid native widget bounds')
    return value['body_hash'], value['body_bytes']


def _same(a, b, keys):
    return all(a[key] == b[key] for key in keys)


def _effects(scenario, lines):
    if (not isinstance(scenario, dict) or not isinstance(scenario.get('steps'), list)
            or not scenario['steps'] or not all(isinstance(s, dict) for s in scenario['steps'])):
        raise ValueError('Invalid native scenario steps')
    effects = _rows(lines, 'qos-native-effect ')
    if len(effects) != len(scenario['steps']):
        raise ValueError('Missing or extra Code Editor effect evidence')
    previous = None
    for index, effect in enumerate(effects):
        if type(effect.get('id')) is not int or effect['id'] != index:
            raise ValueError('Reordered native effect identity')
        _state(effect.get('before'))
        _state(effect.get('after'))
        if previous is not None and not _same(previous, effect['before'], _STATE_KEYS):
            raise ValueError('Discontinuous native state between inputs')
        previous = effect['after']
    return effects


def _input(step, kind, fields):
    return step.get('kind') == kind and set(step) == {'kind', *fields}


def _pointer_scale(scenario, lines):
    """Native input coordinates are physical; observer bounds are logical."""
    if not any(step.get('kind') in ('click', 'hover') for step in scenario['steps']):
        return None
    windows = _rows(lines, 'qos-native-window ')
    if len(windows) != 1:
        raise ValueError('Pointer validation requires exactly one native window metadata row')
    scale = windows[0].get('scale_factor')
    if not _number(scale) or scale <= 0:
        raise ValueError('Pointer validation requires a finite positive native window scale')
    return scale


def _pointer_inside(step, state, scale):
    x, y = step.get('x'), step.get('y')
    if not _number(x) or not _number(y):
        return False
    left, top, right, bottom = state['editor_rect']
    return left <= x / scale < right and top <= y / scale < bottom


def validate_code_editor_typing(scenario, lines, manifest):
    """Require exact alternating prepend-x/backspace durable and caret effects."""
    base, edited = _manifest(manifest)
    effects = _effects(scenario, lines)
    pointer_scale = _pointer_scale(scenario, lines)
    initial = effects[0]['before']
    expected, edits = base, 0
    for step, effect in zip(scenario['steps'], effects):
        before, after = effect['before'], effect['after']
        for state in (before, after):
            if (not _same(state, initial, ('buffer_id', 'root_rect', 'editor_rect'))
                    or state['offset_x'] != 0 or state['offset_y'] != 0):
                raise ValueError('Unexpected identity/geometry/scroll change during typing')
        if _state(before) != expected:
            raise ValueError('Unexpected durable content before edit')
        if _input(step, 'character', ('value',)) and step['value'] == 'x' and expected == base:
            target, from_column, to_column = edited, 0, 1
        elif (_input(step, 'key', ('key',)) and step['key'] == 'backspace'
              and expected == edited):
            target, from_column, to_column = base, 1, 0
        elif edits == 0:
            if _state(after) != base:
                raise ValueError('Setup changed fixture content')
            if _input(step, 'click', ('x', 'y')) and _pointer_inside(step, before, pointer_scale):
                pass
            elif _input(step, 'hover', ('x', 'y')) and _pointer_inside(step, before, pointer_scale):
                if not _same(before, after, _STATE_KEYS):
                    raise ValueError('Hover changed editor state')
            elif _input(step, 'key', ('key',)) and step['key'] == 'home':
                if after['caret_line'] != before['caret_line'] or after['caret_column'] != 0:
                    raise ValueError('Home did not establish the line beginning')
            elif _input(step, 'key', ('key',)) and step['key'] == 'tab':
                pass
            else:
                raise ValueError('Unsupported Code Editor typing setup')
            continue
        else:
            raise ValueError('Unsupported Code Editor typing transition')
        if (not before['editor_focused'] or not after['editor_focused']
                or before['caret_line'] != 0 or after['caret_line'] != 0
                or before['caret_column'] != from_column or after['caret_column'] != to_column
                or _state(after) != target):
            raise ValueError('Edit did not produce exact content, focus, and caret transition')
        expected = target
        edits += 1
    if not edits:
        raise ValueError('No Code Editor edits validated')
    return edits


def validate_code_editor_scroll(scenario, lines, manifest):
    """Require directed vertical wheel movement with unchanged editor content/caret."""
    base, _ = _manifest(manifest)
    effects = _effects(scenario, lines)
    pointer_scale = _pointer_scale(scenario, lines)
    initial = effects[0]['before']
    stable = tuple(key for key in _STATE_KEYS if key != 'offset_y')
    movements = 0
    for step, effect in zip(scenario['steps'], effects):
        before, after = effect['before'], effect['after']
        if (_state(before) != base or _state(after) != base
                or not _same(before, initial, stable) or not _same(after, initial, stable)):
            raise ValueError('Scroll changed content, caret, focus, identity, or viewport')
        delta = after['offset_y'] - before['offset_y']
        if _input(step, 'hover', ('x', 'y')) and _pointer_inside(step, before, pointer_scale):
            if delta != 0:
                raise ValueError('Hover changed scroll position')
        elif (_input(step, 'wheel', ('dx', 'dy')) and _number(step['dx']) and step['dx'] == 0
              and _number(step['dy']) and step['dy'] != 0 and delta * step['dy'] < 0):
            movements += 1
        else:
            raise ValueError('Wheel did not move content in the requested direction')
    if not movements:
        raise ValueError('No Code Editor scroll movement validated')
    return movements


def validate_code_editor_frame_states(scenario, lines):
    """Require every input mapped once, and complete state preserved after drawing."""
    effects = _effects(scenario, lines)
    frames = _rows(lines, 'qos-native-frame ')
    next_input, previous_frame = 0, -1
    for frame in frames:
        identity, inputs = frame.get('cpu_frame_id'), frame.get('input_ids')
        if (type(identity) is not int or identity <= previous_frame
                or not isinstance(inputs, list) or not inputs
                or any(type(i) is not int for i in inputs)
                or inputs != list(range(next_input, next_input + len(inputs)))
                or next_input + len(inputs) > len(effects)):
            raise ValueError('Invalid Code Editor frame/input mapping')
        state = frame.get('state')
        _state(state)
        if not _same(state, effects[inputs[-1]]['after'], _STATE_KEYS):
            raise ValueError('Native editor state did not survive frame construction')
        next_input += len(inputs)
        previous_frame = identity
    if next_input != len(effects):
        raise ValueError('Missing Code Editor postdraw evidence')
    return len(frames)
