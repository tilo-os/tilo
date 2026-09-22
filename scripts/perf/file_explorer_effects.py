"""Validate production Files scroll state; never infer display qualification."""
import json
import math
import re


_STABLE = ('app','view_id','directory','entry_count','order_hash','hash_algorithm',
           'ready','is_loading','has_error','selected_indices','selected_names',
           'primary_index','offset_x','offset_units','root_rect','viewport_rect',
           'view_mode','sort_by','sort_order')
_EPSILON = 0.01  # Logical-pixel tolerance for f32 layout/scroll arithmetic.


def _rows(lines, prefix):
    result = []
    for line in lines:
        if not isinstance(line, str):
            raise ValueError('Invalid log line')
        if line.startswith(prefix):
            value = json.loads(line[len(prefix):])
            if not isinstance(value, dict):
                raise ValueError('Native evidence must be an object')
            result.append(value)
    return result


def _number(value):
    return type(value) in (int,float) and math.isfinite(value)


def _hash(value):
    return isinstance(value,str) and re.fullmatch('[0-9a-f]{64}',value) is not None


def _size(rect):
    return rect[2]-rect[0], rect[3]-rect[1]


def _close(a,b):
    return abs(a-b) <= _EPSILON


def _state(state):
    if (not isinstance(state,dict) or any(k not in state for k in _STABLE)
            or state['app'] != 'dev.qos.file-explorer' or state['view_id'] != 'file_view'
            or state['hash_algorithm'] != 'sha256' or not _hash(state['order_hash'])
            or state['ready'] is not True or state['is_loading'] is not False
            or state['has_error'] is not False
            or not isinstance(state['directory'],str) or not state['directory'].startswith('/')
            or type(state['entry_count']) is not int or state['entry_count'] <= 0
            or state['view_mode'] not in ('List','Grid')
            or state['sort_by'] not in ('Name','Size','Modified','Type')
            or state['sort_order'] not in ('Ascending','Descending')
            or state['offset_units'] != 'pixels'):
        raise ValueError('Invalid or unready Files state')
    for key in ('root_rect','viewport_rect','content_rect'):
        rect=state.get(key)
        if (not isinstance(rect,list) or len(rect)!=4 or not all(_number(v) for v in rect)
                or rect[2]<=rect[0] or rect[3]<=rect[1]):
            raise ValueError('Invalid Files geometry')
    root, viewport = state['root_rect'], state['viewport_rect']
    if (viewport[0]<root[0]-_EPSILON or viewport[1]<root[1]-_EPSILON
            or viewport[2]>root[2]+_EPSILON or viewport[3]>root[3]+_EPSILON):
        raise ValueError('Files viewport lies outside root')
    for key in ('offset_x','offset_y'):
        if not _number(state.get(key)) or state[key]<0:
            raise ValueError('Invalid Files scroll offset')
    if state['offset_y'] > max(0,_size(state['content_rect'])[1]-_size(viewport)[1])+_EPSILON:
        raise ValueError('Files scroll offset exceeds content bounds')
    indices,names,primary=state['selected_indices'],state['selected_names'],state['primary_index']
    if (not isinstance(indices,list) or any(type(i) is not int or not 0<=i<state['entry_count'] for i in indices)
            or len(set(indices))!=len(indices) or not isinstance(names,list)
            or len(names)!=len(indices) or any(not isinstance(n,str) or not n for n in names)
            or (primary is not None and (type(primary) is not int or primary not in indices))):
        raise ValueError('Invalid Files selection')
    if 'thumbnail_pending' not in state or (state['thumbnail_pending'] is not None
            and (type(state['thumbnail_pending']) is not int or state['thumbnail_pending']<0)):
        raise ValueError('Invalid thumbnail pending state')


def _same(a,b):
    return (all(a[k]==b[k] for k in _STABLE)
            and all(_close(x,y) for x,y in zip(_size(a['content_rect']),_size(b['content_rect']))))


def _effects(scenario,lines):
    if (not isinstance(scenario,dict) or not isinstance(scenario.get('steps'),list)
            or not scenario['steps'] or not all(isinstance(s,dict) for s in scenario['steps'])):
        raise ValueError('Invalid Files scenario')
    effects=_rows(lines,'qos-native-effect ')
    if len(effects)!=len(scenario['steps']):
        raise ValueError('Missing or extra Files effects')
    previous=None
    for i,effect in enumerate(effects):
        if type(effect.get('id')) is not int or effect['id']!=i:
            raise ValueError('Reordered Files effects')
        before,after=effect.get('before'),effect.get('after')
        _state(before);_state(after)
        if not _same(before,after):
            raise ValueError('Scroll changed Files identity, content, selection, or layout')
        if previous is not None and (not _same(previous,before) or previous['offset_y']!=before['offset_y']):
            raise ValueError('Discontinuous Files input state')
        previous=after
    return effects


def validate_file_explorer_scroll(scenario,lines,manifest):
    effects=_effects(scenario,lines)
    if (not isinstance(manifest,dict) or type(manifest.get('schema')) is not int or manifest['schema']!=1
            or manifest.get('hash_algorithm')!='sha256' or not _hash(manifest.get('order_hash'))
            or any(type(manifest.get(k)) is not int or manifest[k]<0
                   for k in ('entry_count','image_count','directory_count'))
            or manifest['entry_count']<=0
            or manifest['image_count']+manifest['directory_count']>manifest['entry_count']):
        raise ValueError('Invalid Files fixture manifest')
    for effect in effects:
        state=effect['before']
        if (state['entry_count']!=manifest['entry_count'] or state['order_hash']!=manifest['order_hash']
                or state['sort_by']!='Name' or state['sort_order']!='Ascending'):
            raise ValueError('Files state does not match fixture order')
    windows=_rows(lines,'qos-native-window ')
    if len(windows)!=1 or not _number(windows[0].get('scale_factor')) or windows[0]['scale_factor']<=0:
        raise ValueError('Files pointer requires one actual positive window scale')
    scale=windows[0]['scale_factor']
    movements=0
    for step,effect in zip(scenario['steps'],effects):
        before,after=effect['before'],effect['after']
        delta=after['offset_y']-before['offset_y']
        if step.get('kind')=='hover' and set(step)=={'kind','x','y'}:
            left,top,right,bottom=before['viewport_rect']
            if (not _number(step['x']) or not _number(step['y']) or delta!=0
                    or not left<=step['x']/scale<right or not top<=step['y']/scale<bottom):
                raise ValueError('Files hover missed viewport or changed scroll')
        elif (set(step)=={'kind','dx','dy'} and step.get('kind')=='wheel'
                and _number(step['dx']) and step['dx']==0 and _number(step['dy'])
                and step['dy']!=0 and delta*step['dy']<0):
            movements+=1
        else:
            raise ValueError('Files wheel did not move in the requested direction')
    if not movements:
        raise ValueError('No Files scroll movements')
    return movements


def validate_file_explorer_frame_states(scenario,lines):
    effects=_effects(scenario,lines)
    initial=effects[0]['before']
    origin=[initial['content_rect'][axis]+initial[key]-initial['viewport_rect'][axis]
            for axis,key in ((0,'offset_x'),(1,'offset_y'))]
    frames=_rows(lines,'qos-native-frame ')
    next_input,previous_frame=0,-1
    for frame in frames:
        identity,inputs=frame.get('cpu_frame_id'),frame.get('input_ids')
        if (type(identity) is not int or identity<=previous_frame or not isinstance(inputs,list)
                or not inputs or any(type(i) is not int for i in inputs)
                or inputs!=list(range(next_input,next_input+len(inputs)))
                or next_input+len(inputs)>len(effects)):
            raise ValueError('Invalid Files frame/input mapping')
        state=frame.get('state');_state(state)
        after=effects[inputs[-1]]['after']
        if not _same(state,after) or state['offset_y']!=after['offset_y']:
            raise ValueError('Files input state did not survive frame construction')
        for baseline,(axis,key) in zip(origin,((0,'offset_x'),(1,'offset_y'))):
            if not _close(state['content_rect'][axis]+state[key]-state['viewport_rect'][axis],baseline):
                raise ValueError('Files content geometry does not reflect scroll offset')
        next_input+=len(inputs);previous_frame=identity
    if next_input!=len(effects):
        raise ValueError('Missing Files postdraw evidence')
    return len(frames)
