"""Exact default Settings search workload; state evidence is not display proof.

Contract: settled normal Network pane, wide visible sidebar, empty initial
search. Warmup may focus by clicking the actual field or hover it. All edits
alternate 'z'/Backspace. Results below are a fixed fixture contract sourced from
apps/settings/src/categories.rs (ALL, slug, keywords, subtitle) and sidebar.rs.
Changing the catalogue requires explicitly reviewing this contract.
"""
import json
import math
import re

ALL_CATEGORIES = ('network','bluetooth','devices_sync','sound','speakers','displays','mouse_keyboard',
    'keyboard_shortcuts','printers','appearance','accessibility','notifications','default-apps',
    'storage','power','time_date','users','region_formats','external_accounts','agent_models',
    'permissions','updates','about')
Z_CATEGORIES = ('appearance','accessibility','time_date')
_STABLE = ('app','search_id','ready','search_rect','root_rect','selected_category','narrow',
           'sidebar_open','document_root','document_actor','document_witness_kind','effective_document_heads')


def _rows(lines,prefix):
    result=[]
    for line in lines:
        if not isinstance(line,str):raise ValueError('Invalid log line')
        if line.startswith(prefix):
            value=json.loads(line[len(prefix):])
            if not isinstance(value,dict):raise ValueError('Settings evidence must be an object')
            result.append(value)
    return result


def _number(value):
    return type(value) in (int,float) and math.isfinite(value)


def _state(state):
    if (not isinstance(state,dict) or any(k not in state for k in _STABLE)
        or state['app']!='dev.tilo.settings' or state['search_id']!='settings.search'
        or state['ready'] is not True or state['selected_category']!='network'
        or state['narrow'] is not False or state['sidebar_open'] is not True
        or state['document_witness_kind']!='effective_heads_on_disposable_clone'
        or not isinstance(state['document_root'],str) or not state['document_root']
        or not isinstance(state['document_actor'],str)
        or re.fullmatch('(?:[0-9a-f]{2})+',state['document_actor']) is None):
        raise ValueError('Invalid or unready Settings state')
    heads=state['effective_document_heads']
    if (not isinstance(heads,list) or not heads
        or any(not isinstance(h,str) or re.fullmatch('[0-9a-f]{64}',h) is None for h in heads)
        or heads!=sorted(set(heads))):raise ValueError('Invalid effective document heads')
    pending=state.get('original_pending_ops')
    if type(pending) is not int or pending<0:raise ValueError('Invalid pending operation count')
    for key in ('search_rect','root_rect'):
        rect=state[key]
        if (not isinstance(rect,list) or len(rect)!=4 or not all(_number(v) for v in rect)
                or rect[2]<=rect[0] or rect[3]<=rect[1]):raise ValueError('Invalid Settings bounds')
    root,search=state['root_rect'],state['search_rect']
    if not (root[0]<=search[0]<search[2]<=root[2] and root[1]<=search[1]<search[3]<=root[3]):
        raise ValueError('Search field outside root')
    local,query=state.get('local_search_text'),state.get('synchronized_search_query')
    if local not in ('','z') or query not in ('','z'):raise ValueError('Unexpected Settings search text')
    focused=state.get('search_focused')
    if type(focused) is not bool:raise ValueError('Missing Settings search focus')
    for key in ('search_widget_text','search_caret_byte'):
        if key not in state:raise ValueError('Missing Settings widget state')
    if focused:
        if (state['search_widget_text']!=local or type(state['search_caret_byte']) is not int
            or state['search_caret_byte']!=len(local)):raise ValueError('Wrong Settings text/caret')
    elif state['search_widget_text'] is not None or state['search_caret_byte'] is not None:
        raise ValueError('Unfocused Settings observer must not invent text/caret')
    expected=list(ALL_CATEGORIES if query=='' else Z_CATEGORIES)
    if (state.get('rendered_category_ids')!=expected
        or type(state.get('rendered_result_count')) is not int
        or state['rendered_result_count']!=len(expected)):
        raise ValueError('Settings result set differs from exact catalogue contract')


def _same(a,b):return all(a[k]==b[k] for k in _STABLE)


def _pending_transition(before,after):
    # Autosave may close the already-observed transaction between callbacks.
    return after in (0,before)


def _effects(scenario,lines):
    if (not isinstance(scenario,dict) or not isinstance(scenario.get('steps'),list)
        or not scenario['steps'] or not all(isinstance(s,dict) for s in scenario['steps'])):
        raise ValueError('Invalid Settings scenario')
    effects=_rows(lines,'tilo-native-effect ')
    if len(effects)!=len(scenario['steps']):raise ValueError('Missing or extra Settings effects')
    previous=None
    for identity,effect in enumerate(effects):
        if type(effect.get('id')) is not int or effect['id']!=identity:raise ValueError('Reordered Settings effects')
        before,after=effect.get('before'),effect.get('after')
        _state(before);_state(after)
        if (not _same(before,after) or before['original_pending_ops']!=after['original_pending_ops']
            or before['synchronized_search_query']!=after['synchronized_search_query']):
            raise ValueError('Immediate search changed durable state, geometry, or synchronized results')
        if previous is not None and (not _same(previous,before)
            or previous['local_search_text']!=before['local_search_text']
            or previous['search_focused']!=before['search_focused']
            or before['synchronized_search_query'] not in (previous['synchronized_search_query'],previous['local_search_text'])
            or not _pending_transition(previous['original_pending_ops'],before['original_pending_ops'])):
            raise ValueError('Discontinuous Settings state or document mutation')
        previous=after
    initial=effects[0]['before']
    if initial['local_search_text']!='' or initial['synchronized_search_query']!='':
        raise ValueError('Settings workload must start with empty settled search')
    return effects


def validate_settings_typing(scenario,lines):
    effects=_effects(scenario,lines)
    warmup=scenario.get('warmup_steps',0)
    if type(warmup) is not int or not 0<=warmup<len(effects):raise ValueError('Invalid warmup boundary')
    windows=_rows(lines,'tilo-native-window ')
    if len(windows)!=1 or not _number(windows[0].get('scale_factor')) or windows[0]['scale_factor']<=0:
        raise ValueError('Settings pointer requires actual positive native scale')
    scale=windows[0]['scale_factor'];edits=0
    for index,(step,effect) in enumerate(zip(scenario['steps'],effects)):
        before,after=effect['before'],effect['after']
        if step=={'kind':'character','value':'z'} and before['local_search_text']=='':
            target='z'
        elif step=={'kind':'key','key':'backspace'} and before['local_search_text']=='z':
            target=''
        elif index<warmup and edits==0 and set(step)=={'kind','x','y'} and step['kind'] in ('click','hover'):
            x,y=step['x'],step['y'];left,top,right,bottom=before['search_rect']
            if (not _number(x) or not _number(y) or not left<=x/scale<right or not top<=y/scale<bottom
                or after['local_search_text']!=before['local_search_text']
                or (step['kind']=='click' and not after['search_focused'])
                or (step['kind']=='hover' and before['search_focused']!=after['search_focused'])):
                raise ValueError('Invalid Settings search setup')
            continue
        else:raise ValueError('Unsupported or ineffective Settings search input')
        if not before['search_focused'] or not after['search_focused'] or after['local_search_text']!=target:
            raise ValueError('Settings edit did not retain focus and produce expected text')
        edits+=1
    if not edits:raise ValueError('No Settings edits verified')
    return edits


def validate_settings_frame_states(scenario,lines):
    effects=_effects(scenario,lines);frames=_rows(lines,'tilo-native-frame ')
    next_input,previous_frame=0,-1
    for frame in frames:
        identity,inputs=frame.get('cpu_frame_id'),frame.get('input_ids')
        if (type(identity) is not int or identity<=previous_frame or not isinstance(inputs,list) or not inputs
            or any(type(i) is not int for i in inputs) or inputs!=list(range(next_input,next_input+len(inputs)))
            or next_input+len(inputs)>len(effects)):raise ValueError('Invalid Settings frame/input mapping')
        state=frame.get('state');_state(state);after=effects[inputs[-1]]['after']
        if (not _same(state,after) or state['local_search_text']!=after['local_search_text']
            or state['synchronized_search_query']!=after['local_search_text']
            or state['search_focused']!=after['search_focused']
            or not _pending_transition(after['original_pending_ops'],state['original_pending_ops'])):
            raise ValueError('Settings edit or document state did not survive frame construction')
        next_input+=len(inputs);previous_frame=identity
    if next_input!=len(effects):raise ValueError('Missing Settings postdraw evidence')
    return len(frames)
