"""Validate native Scratchpad effects against an independently seeded note."""
import math
import re
from native_delivery import rows

def _manifest(manifest):
    for key in ('body_hash', 'prefixed_x_hash'):
        value = manifest.get(key)
        if not isinstance(value, str) or not re.fullmatch('[0-9a-f]{64}', value):
            raise ValueError('Invalid fixture content hash')
    for key in ('body_bytes', 'prefixed_x_bytes'):
        if type(manifest.get(key)) is not int or manifest[key] <= 0:
            raise ValueError('Invalid fixture byte count')
    if (manifest['prefixed_x_bytes'] != manifest['body_bytes'] + 1
            or manifest['prefixed_x_hash'] == manifest['body_hash']):
        raise ValueError('Invalid fixture edit transition')
    return ((manifest['body_hash'],manifest['body_bytes']),
            (manifest['prefixed_x_hash'],manifest['prefixed_x_bytes']))

def _state(value):
    if not isinstance(value,dict) or value.get('ready') is not True:
        raise ValueError('Native fixture is not ready')
    for key in ('root_rect','editor_rect','scroll_rect'):
        rect=value.get(key)
        if (not isinstance(rect,list) or len(rect)!=4
                or any(type(v) not in (int,float) or not math.isfinite(v) for v in rect)
                or rect[2]<=rect[0] or rect[3]<=rect[1]):
            raise ValueError('Invalid native widget bounds')
    for key in ('offset_x','offset_y'):
        if type(value.get(key)) not in (int,float) or not math.isfinite(value[key]) or value[key]<0:
            raise ValueError('Invalid native scroll offset')
    if type(value.get('body_bytes')) is not int:
        raise ValueError('Invalid native content length')
    return value.get('body_hash'),value['body_bytes']

def _effects(scenario,lines):
    effects=rows(lines,'qos-native-effect ')
    if not scenario.get('steps') or len(effects)!=len(scenario['steps']):
        raise ValueError('Missing or extra Scratchpad effect evidence')
    for index,effect in enumerate(effects):
        if type(effect.get('id')) is not int or effect['id']!=index:
            raise ValueError('Reordered Scratchpad effect identity')
    return effects

def validate_scratchpad_typing(scenario,lines,manifest):
    base,edited=_manifest(manifest)
    effects=_effects(scenario,lines)
    expected=base
    edits=0
    root_rect=None
    for step,effect in zip(scenario['steps'],effects):
        before,after=effect.get('before'),effect.get('after')
        actual_before,actual_after=_state(before),_state(after)
        if root_rect is None:
            root_rect=before['root_rect']
        for state in (before,after):
            if state['root_rect']!=root_rect or state['offset_x']!=0 or state['offset_y']!=0:
                raise ValueError('Unexpected viewport change during fixed-view typing')
        if actual_before!=expected:
            raise ValueError('Discontinuous Scratchpad content')
        if step.get('kind')=='character' and step.get('value')=='x' and expected==base:
            expected=edited
        elif step.get('kind')=='key' and step.get('key')=='backspace' and expected==edited:
            expected=base
        elif edits==0 and (
                (step.get('kind')=='key' and step.get('key') in ('tab','home'))
                or step.get('kind')=='click'):
            if actual_after!=base:
                raise ValueError('Setup changed fixture content')
            continue
        else:
            raise ValueError('Unsupported Scratchpad typing transition')
        if before.get('editor_focused') is not True or after.get('editor_focused') is not True:
            raise ValueError('Native editor is not focused')
        if actual_after!=expected:
            raise ValueError('Requested edit did not produce the fixture transition')
        edits+=1
    if not edits:
        raise ValueError('No Scratchpad edits validated')
    return edits

def validate_scratchpad_scroll(scenario,lines,manifest):
    base,_=_manifest(manifest)
    effects=_effects(scenario,lines)
    previous=None
    root_rect=None
    movements=0
    for step,effect in zip(scenario['steps'],effects):
        before,after=effect.get('before'),effect.get('after')
        if _state(before)!=base or _state(after)!=base:
            raise ValueError('Scrolling changed fixture content')
        if root_rect is None:
            root_rect=before['root_rect']
        if before['root_rect']!=root_rect or after['root_rect']!=root_rect:
            raise ValueError('Viewport changed during scrolling')
        if previous is not None and before['offset_y']!=previous:
            raise ValueError('Discontinuous scroll position')
        if before['offset_x']!=0 or after['offset_x']!=0:
            raise ValueError('Unexpected horizontal scroll')
        delta=after['offset_y']-before['offset_y']
        if step.get('kind')=='hover':
            if delta!=0:
                raise ValueError('Hover changed scroll position')
        elif (step.get('kind')=='wheel' and type(step.get('dy')) in (int,float)
              and math.isfinite(step['dy']) and step['dy']!=0 and delta*step['dy']<0):
            movements+=1
        else:
            raise ValueError('Wheel input did not move content in the requested direction')
        previous=after['offset_y']
    if not movements:
        raise ValueError('No Scratchpad scroll movement validated')
    return movements

def validate_scratchpad_frame_states(scenario,lines):
    """Verify state survived drawing; CPU/worker correlation is a separate check."""
    effects=_effects(scenario,lines)
    frames=rows(lines,'qos-native-frame ')
    next_input=0
    previous_frame=-1
    keys=('body_hash','body_bytes','offset_x','offset_y','editor_focused','root_rect')
    for frame in frames:
        identity,inputs=frame.get('cpu_frame_id'),frame.get('input_ids')
        if (type(identity) is not int or identity<=previous_frame
                or not isinstance(inputs,list) or not inputs
                or any(type(i) is not int for i in inputs)
                or inputs!=list(range(next_input,next_input+len(inputs)))
                or next_input+len(inputs)>len(effects)):
            raise ValueError('Invalid native frame input mapping')
        state=frame.get('state')
        _state(state)
        expected=effects[inputs[-1]]['after']
        if any(state.get(key)!=expected.get(key) for key in keys):
            raise ValueError('Native effect did not survive frame construction')
        next_input+=len(inputs)
        previous_frame=identity
    if next_input!=len(effects):
        raise ValueError('Missing native frame state')
    return len(frames)
