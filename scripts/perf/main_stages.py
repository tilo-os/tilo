"""Optional post-draw main-thread timings; not a complete frame/GPU metric."""
from native_delivery import rows
from render_boundaries import _bracket, _ordered, _interval, summarize_render_boundaries
from runtime_benchmark import distribution

POINTS=('before_profiler_commit','after_profiler_commit','after_native_frame_log',
        'after_debug_outlines','after_damage_mark_drawn','after_accessibility',
        'after_hud_request','after_native_redraw_log')
STAGES=('profiler_commit','native_frame_log','debug_outlines','damage_mark_drawn',
        'accessibility','hud_request','native_redraw_log')

def summarize_main_stages(lines, expected_cpu_ids):
    expected=set(expected_cpu_ids)
    if (not expected or len(expected)!=len(expected_cpu_ids)
            or any(type(i) is not int or i<0 for i in expected)):
        raise ValueError('Invalid expected main-stage CPU identities')
    evidence=rows(lines,'qos-native-main-stages ')
    if not evidence:
        return dict(available=False,performance_certified=False,
                    reason='Main-stage boundaries unavailable')
    # Require the same complete positive worker/display identity chain.
    if not summarize_render_boundaries(lines,expected_cpu_ids)['available']:
        raise ValueError('Main-stage evidence lacks render boundaries')
    sends={r['cpu_frame_id']:r for r in rows(lines,'qos-native-send ')}
    by_cpu={}
    for row in evidence:
        cpu=row.get('cpu_frame_id')
        if ('cpu_frame_id' not in row or
                (cpu is not None and (type(cpu) is not int or cpu<0 or cpu in by_cpu))):
            raise ValueError('Invalid or duplicate main-stage CPU identity')
        brackets=[_bracket(row,p) for p in POINTS]
        _ordered(*brackets)
        if cpu is not None:
            by_cpu[cpu]=brackets
    if not expected<=by_cpu.keys():
        raise ValueError('Missing main-stage boundaries')
    frames=[]
    for cpu in sorted(expected):
        brackets=by_cpu[cpu]
        send=_bracket(sends[cpu],'before')
        _ordered(*brackets,send)
        durations={name:_interval(a,b) for name,a,b in zip(STAGES,brackets,brackets[1:])}
        durations['pre_send_gap']=_interval(brackets[-1],send)
        durations['post_draw_to_send']=_interval(brackets[0],send)
        frames.append(dict(cpu_frame_id=cpu,intervals_ms=durations))
    stages={name:{bound:distribution([f['intervals_ms'][name][i] for f in frames],scale=1)
                  for i,bound in enumerate(('lower','upper'))}
            for name in frames[0]['intervals_ms']}
    return dict(available=True,performance_certified=False,frames=frames,stages=stages,
                limitations=['Post-draw region only; excludes earlier input/layout/drawing and channel-send duration.',
                             'Native snapshot/logging is included; scheduler preemption can affect every stage.',
                             'Host sample bounds do not bound timestamp API accuracy or identify GPU completion.'])
