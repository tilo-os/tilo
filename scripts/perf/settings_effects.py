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
Z_CATEGORIES = ('mouse_keyboard','appearance','accessibility','time_date')
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
        or state['app']!='so.tilo.settings' or state['search_id']!='settings.search'
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


def validate_navigation_read(value, pane):
    """Validate mounted state, without treating composition as presentation."""
    if (not isinstance(value, dict) or value.get('app') != 'so.tilo.settings'
            or value.get('selected_pane') != pane or value.get('requested_pane') != pane
            or value.get('path') != 'settings://' + pane or value.get('pending') is not False
            or value.get('ready') is not True or value.get('content_visible') is not True
            or value.get('presentation_attribution_available') is not False):
        raise ValueError('Requested Settings navigation is not observably ready')
    return dict(selected_pane=pane, path=value['path'], navigation_verified=True,
                latency_available=False)


def settings_search_navigation(vm, bound, scope, call, query='keyboard'):
    """Run only on an already pinned fresh owner window through maintained APIs.

    ``scope`` must revalidate the authenticated host tab/record/native lifetime
    and preserved user tabs; ``call`` is the existing tracked action_jobs invoke
    adapter. The caller prepares/focuses its fresh window, never an app-only
    selection. This does not provide hover or input-to-KMS latency evidence.
    """
    import time
    if (not isinstance(bound, tuple) or len(bound) != 3
            or bound[0] != 'so.tilo.settings' or not all(bound)
            or query != 'keyboard'):
        raise ValueError('Exact owned Settings binding and fixture query required')
    scope()
    def row(resource, operation):
        response = vm.control_json('actions', 'list', bound[0], '--programmatic', '--json')
        if response.get('query') != bound[0] or not isinstance(response.get('actions'), list):
            raise ValueError('Matched Settings action discovery unavailable')
        rows = response['actions']
        found = [r for r in rows if r.get('provider') == 'app-actions'
                 and tuple(r.get(k) for k in ('application', 'room', 'record')) == bound
                 and r.get('semantic_identity') == dict(kind=('settings.section' if resource.startswith('settings.section:') else 'settings.navigation'), key=resource)
                 and r.get('operation') == operation
                 and r.get('contract', {}).get('availability', {}).get('state') == 'available']
        if len(found) != 1 or not found[0].get('id'):
            raise ValueError('Exact available Settings operation missing or ambiguous')
        return found[0]

    target = row('settings.section:mouse_keyboard', 'open-pane')
    row('settings.navigation', 'inspect-navigation')  # Refuse missing Read before input.
    scope()
    vm.key('CTRL+K')
    vm.input('type', query)
    expected = {k: target[k] for k in ('provider', 'id')}
    deadline = time.monotonic() + 10
    visited = set()
    while True:
        scope()
        palette = vm.state().get('chrome', {}).get('palette', {})
        if palette.get('visible') is not True:
            raise ValueError('Settings search palette unavailable')
        if palette.get('query') != query:
            if time.monotonic() >= deadline:
                raise ValueError('Settings search query was not adopted')
            time.sleep(.1)
            continue
        selected = palette.get('selected_action')
        if selected == expected:
            break
        if time.monotonic() >= deadline:
            raise ValueError('Exact owned search route unavailable; Enter was not sent')
        if selected is None:
            time.sleep(.1)
            continue
        identity = json.dumps(selected, sort_keys=True)
        if identity in visited or len(visited) >= 32:
            raise ValueError('Search traversal did not reach the exact owned route')
        visited.add(identity)
        vm.key('ArrowDown')
    scope()
    # Recheck exact selection immediately before the single activation.
    if ({k: row('settings.section:mouse_keyboard', 'open-pane')[k] for k in ('provider', 'id')} != expected
            or vm.state().get('chrome', {}).get('palette', {}).get('selected_action') != expected):
        raise ValueError('Selected search action changed before Enter')
    vm.key('Enter')
    for attempt in range(20):
        scope()
        inspector = row('settings.navigation', 'inspect-navigation')
        scope()
        value = call(inspector, {}, 'settings-navigation-' + str(attempt))
        try:
            return validate_navigation_read(value, 'mouse_keyboard')
        except ValueError:
            if not isinstance(value, dict) or value.get('app') != bound[0]:
                raise
            time.sleep(.1)
    raise ValueError('Selected Settings pane remained pending')


def summarize_settings_activation(lines, invocation, pane, process_id, observer_session):
    """Join an admitted broker UUID to actual Settings draw and KMS feedback.

    This measures activation only, not palette opening/query or native hover.
    The caller must pin the fresh authenticated host worker and mapped SDK
    before capture; namespace PID alone cannot establish worker custody.
    """
    import uuid
    if (pane not in ALL_CATEGORIES or type(process_id) is not int or process_id <= 0
            or any(not isinstance(value, str) for value in (invocation, observer_session))):
        raise ValueError('Exact Settings invocation, pane and worker identity required')
    for value in (invocation, observer_session):
        parsed = uuid.UUID(value)
        if str(parsed) != value or parsed.version != 4:
            raise ValueError('Canonical random invocation/observer UUID required')
    submissions = [r for r in _rows(lines, 'tilo-action-submission ')
                   if r.get('invocation') == invocation]
    inputs = [r for r in _rows(lines, 'tilo-broker-input ')
              if r.get('invocation') == invocation]
    if len(submissions) != 1 or len(inputs) != 1:
        raise ValueError('Missing or duplicate exact Settings activation')
    submitted, event = submissions[0], inputs[0]
    owner = (process_id, observer_session)
    def owned(row):
        return (row.get('process_id'), row.get('observer_session')) == owner
    if (submitted.get('application') != 'so.tilo.settings'
            or type(submitted.get('clock_id')) is not int or submitted.get('clock_id') != 1
            or not owned(event) or event.get('handler_returned_ok') is not True
            or event.get('clock') != 'clock_monotonic_seconds'):
        raise ValueError('Settings invocation was not handled by the exact worker')
    start = submitted.get('submission_time_ns')
    if type(start) is not int or start < 0:
        raise ValueError('Missing monotonic submission time')
    times = [event.get('handler_' + point + '_' + end + '_s')
             for point in ('dispatch', 'return') for end in ('lo', 'hi')]
    if (not all(_number(t) and t >= 0 for t in times)
            or times != sorted(times) or start > times[0] * 1e9):
        raise ValueError('Invalid submission/handler clock ordering')
    frames = [r for r in _rows(lines, 'tilo-broker-frame ')
              if invocation in r.get('invocations', [])]
    if not frames or any(not owned(r) or r.get('invocations') != [invocation] for r in frames):
        raise ValueError('Missing, foreign or coalesced Settings invocation draw')
    ids = [r.get('cpu_frame_id') for r in frames]
    if (any(type(i) is not int or i < 0 for i in ids) or len(set(ids)) != len(ids)):
        raise ValueError('Invalid or duplicate Settings draw frame')
    feedback = {}
    for row in _rows(lines, 'tilo-display-frame '):
        if not owned(row):
            continue
        cpu = row.get('cpu_frame_id')
        if cpu not in ids:
            continue
        if cpu in feedback or row.get('backend') != 'wayland':
            raise ValueError('Duplicate or non-Wayland exact frame feedback')
        feedback[cpu] = row
    if set(feedback) != set(ids):
        raise ValueError('Missing exact Settings frame feedback')
    selected = None
    for frame in sorted(frames, key=lambda r: r['cpu_frame_id']):
        before, after, drawn = event.get('before'), event.get('after'), frame.get('state')
        for snapshot in (before, after, drawn):
            if not isinstance(snapshot, dict) or snapshot.get('app') != 'so.tilo.settings':
                raise ValueError('Settings navigation witness missing')
        for snapshot in (before, drawn):
            current = snapshot.get('navigation')
            if (not isinstance(current, dict) or current != snapshot.get('rendered_navigation')
                    or current.get('ready') is not True or current.get('pending') is not False
                    or current.get('content_visible') is not True
                    or current.get('requested_pane') != current.get('selected_pane')
                    or current.get('path') != 'settings://' + str(current.get('selected_pane'))):
                raise ValueError('Actual current and drawn Settings pane disagree')
        if (before['navigation']['selected_pane'] == pane
                or drawn['navigation']['selected_pane'] != pane
                or after.get('navigation', {}).get('requested_pane') != pane):
            raise ValueError('This invocation did not request and draw the intended changed pane')
        display = feedback[frame['cpu_frame_id']]
        if display.get('discarded') is True:
            if selected is not None:
                continue
            continue  # Explicit negative outcome, never credited as presentation.
        if (display.get('discarded') is not False or display.get('clock_id') != 1
                or type(display.get('flags')) is not int or display['flags'] & 7 != 7
                or type(display.get('presented_time_ns')) is not int
                or type(display.get('sequence')) is not int or display['sequence'] < 0):
            raise ValueError('Settings draw lacks proven monotonic hardware presentation')
        if selected is None:
            selected = frame, display
    if selected is None:
        raise ValueError('No presented changed Settings pane')
    frame, display = selected
    end = display['presented_time_ns']
    proof = end, display['sequence']
    if sum((r.get('presented_time_ns'), r.get('sequence')) == proof
           and r.get('discarded') is False for r in feedback.values()) != 1:
        raise ValueError('Ambiguous Settings physical outcome')
    if end < times[-1] * 1e9:
        raise ValueError('KMS outcome precedes handler completion')
    source, original = submitted.get('source'), submitted.get('input_time_ns')
    physical = source in ('physical_key', 'physical_pointer_button')
    dispatch = submitted.get('activation_dispatch_ns')
    if physical:
        if (type(original) is not int or type(dispatch) is not int
                or not 0 <= original <= dispatch <= start
                or (source == 'physical_pointer_button' and original == 0)):
            raise ValueError('Missing trusted original physical activation time')
    elif source not in ('semantic', 'controlled_input', 'native_unstamped') or original is not None:
        raise ValueError('Unsupported or forged physical activation origin')
    return dict(invocation=invocation, process_id=process_id, observer_session=observer_session,
                pane=pane, cpu_frame_id=frame['cpu_frame_id'], physical_sequence=display['sequence'],
                presented_time_ns=end, source=source, physical_activation_available=physical,
                submission_to_kms_ms=(end-start)/1e6,
                physical_activation_to_kms_ms=(end-original)/1e6 if physical else None,
                palette_open_latency_available=False, hover_latency_available=False)



def activation_lines(text):
    """Normalize bounded existing flight rows; export no unrelated log content."""
    prefixes = ('tilo-action-submission ', 'tilo-broker-input ',
                'tilo-broker-frame ', 'tilo-display-frame ')
    if not isinstance(text, str) or len(text.encode()) > 8 * 1024 * 1024:
        raise ValueError('Settings flight evidence exceeds its bound')
    result = []
    for line in text.splitlines():
        found = [prefix for prefix in prefixes if prefix in line]
        if not found:
            continue
        if len(found) != 1 or len(result) >= 8192:
            raise ValueError('Ambiguous or excessive Settings trace rows')
        prefix = found[0]
        payload = line.partition(prefix)[2]
        if len(payload.encode()) > 8192:
            raise ValueError('Settings trace row exceeds its bound')
        row = json.loads(payload)
        if not isinstance(row, dict):
            raise ValueError('Settings trace must be an object')
        result.append(prefix + json.dumps(row))
    return result


def activation_main():
    """Offline only: no device connection, input, navigation or state mutation."""
    import argparse
    from pathlib import Path
    parser = argparse.ArgumentParser(description='Exact Settings activation-to-pane KMS report (offline)')
    parser.add_argument('--log', required=True, type=Path)
    parser.add_argument('--invocation', required=True)
    parser.add_argument('--pane', required=True, choices=ALL_CATEGORIES)
    parser.add_argument('--process-id', required=True, type=int)
    parser.add_argument('--observer-session', required=True)
    args = parser.parse_args()
    if args.log.stat().st_size > 8 * 1024 * 1024:
        raise ValueError('Settings evidence exceeds the bounded offline capture')
    lines = activation_lines(args.log.read_text())
    report = summarize_settings_activation(lines, args.invocation,
                                          args.pane, args.process_id, args.observer_session)
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    activation_main()
