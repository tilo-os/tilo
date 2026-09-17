"""Same-host-clock CPU render boundaries; no GPU completion claim."""
import math
from native_delivery import rows
from presentation_feedback import summarize_presentation
from runtime_benchmark import distribution


def _bracket(row, prefix):
    values=[row.get(prefix+'_host_'+end+'_s') for end in ('lo','hi')]
    if (row.get('host_clock') != 'mach_absolute_seconds'
            or any(type(v) not in (int,float) or not math.isfinite(v) or v <= 0 for v in values)
            or values[1] < values[0]):
        raise ValueError('Invalid render boundary host-clock bracket')
    return values


def _interval(start, end):
    low=math.nextafter(end[0],-math.inf)-math.nextafter(start[1],math.inf)
    high=math.nextafter(end[1],math.inf)-math.nextafter(start[0],-math.inf)
    result=[math.nextafter(math.nextafter(low,-math.inf)*1000,-math.inf),
            math.nextafter(math.nextafter(high,math.inf)*1000,math.inf)]
    if not all(math.isfinite(v) for v in result):
        raise ValueError('Render boundary interval overflow')
    return result


def _ordered(*brackets):
    if any(a[1] > b[0] for a,b in zip(brackets,brackets[1:])):
        raise ValueError('Render boundary causal order is reversed')


def summarize_render_boundaries(lines, expected_cpu_ids):
    expected=set(expected_cpu_ids)
    if (not expected or len(expected)!=len(expected_cpu_ids)
            or any(type(i) is not int or i<0 for i in expected)):
        raise ValueError('Invalid expected render CPU identities')
    groups=[rows(lines,prefix) for prefix in ('qos-native-send ','qos-native-worker ',
                                            'qos-native-present-call ')]
    if not any(groups):
        return dict(available=False,performance_certified=False,
                    reason='Common-clock render boundaries unavailable')
    maps=[]
    for group_index,group in enumerate(groups):
        by_cpu={}
        worker_keys=set()
        for row in group:
            cpu=row.get('cpu_frame_id')
            if ('cpu_frame_id' not in row
                    or (cpu is not None and (type(cpu) is not int or cpu<0 or cpu in by_cpu))):
                raise ValueError('Invalid or duplicate render boundary identity')
            names=(('receive','render_start','render_end') if group_index==1 else ('before','after'))
            if group_index==1 and row.get('outcome')=='SurfaceGone':
                render_fields=[name+'_host_'+end+'_s' for name in ('render_start','render_end')
                               for end in ('lo','hi')]
                if all(key in row and row[key] is None for key in render_fields):
                    # A resize failure occurs before render_one_frame. It is
                    # valid diagnostic evidence, never a presented measurement.
                    names=('receive',)
            _ordered(*[_bracket(row,name) for name in names])
            if group_index==0:
                if type(row.get('send_ok')) is not bool:
                    raise ValueError('Missing send result')
            else:
                if any(type(row.get(k)) is not int or row[k]<0 for k in ('worker_id','worker_frame_ix')):
                    raise ValueError('Invalid worker boundary identity')
                key=row['worker_id'],row['worker_frame_ix']
                if key in worker_keys:
                    raise ValueError('Duplicate worker boundary identity')
                worker_keys.add(key)
                if group_index==1 and row.get('outcome') not in ('Presented','Skipped','Occluded','Reconfigured','SurfaceGone'):
                    raise ValueError('Invalid worker outcome')
            # Auxiliary/unprofiled frames have explicit null CPU identity;
            # validate their shape but never alias them to a measured frame.
            if cpu is not None:
                by_cpu[cpu]=row
        if not expected <= by_cpu.keys():
            raise ValueError('Missing render boundaries for expected CPU frames')
        maps.append(by_cpu)
    feedback=summarize_presentation(lines,expected_cpu_ids)
    if not feedback['feedback_complete']:
        raise ValueError('Incomplete positive display evidence for render boundaries')
    callbacks={r['cpu_frame_id']:r for r in rows(lines,'qos-display-frame ')}
    frames=[]
    for cpu in sorted(expected):
        send,worker,present=[group[cpu] for group in maps]
        display=callbacks[cpu]
        for row in (worker,present):
            if any(type(row.get(k)) is not int or row[k]<0 or row[k]!=display[k]
                   for k in ('worker_id','worker_frame_ix')):
                raise ValueError('Render boundary worker identity differs from display')
        if send.get('send_ok') is not True or worker.get('outcome')!='Presented':
            raise ValueError('Expected render frame failed to send or present')
        before,after=[_bracket(send,p) for p in ('before','after')]
        receive,start,end=[_bracket(worker,p) for p in ('receive','render_start','render_end')]
        present_before,present_after=[_bracket(present,p) for p in ('before','after')]
        shown=[display['presented_time_s']]*2
        _ordered(before,after)
        _ordered(before,receive,start,present_before,present_after,end)
        if shown[1] < present_before[0]:
            raise ValueError('Display precedes actual present call')
        # Receive may precede send return. Display may precede present return.
        pairs=dict(send_call=(before,after),send_start_to_receive=(before,receive),
                   receive_to_render_start=(receive,start),worker_render_cpu=(start,end),
                   render_start_to_present_call=(start,present_before),
                   present_call_cpu=(present_before,present_after),
                   present_return_to_display=(present_after,shown),
                   send_start_to_display=(before,shown))
        frames.append(dict(cpu_frame_id=cpu,worker_id=worker['worker_id'],
                           worker_frame_ix=worker['worker_frame_ix'],
                           intervals_ms={key:_interval(*pair) for key,pair in pairs.items()}))
    stages={key:{bound:distribution([f['intervals_ms'][key][index] for f in frames],scale=1)
                 for index,bound in enumerate(('lower','upper'))}
            for key in frames[0]['intervals_ms']}
    return dict(available=True,performance_certified=False,frames=frames,stages=stages,
                limitations=['CPU boundaries do not measure GPU completion or isolate compositor delay.',
                             'Worker CPU duration includes diagnostic logging and device.poll; instrumentation perturbs timing.',
                             'Send start to receive includes bounded-channel waiting; receive can precede send return.',
                             'Negative present-return-to-display means display occurred before CPU return.',
                             'Bounds cover sampled host readings and numeric representation, not timestamp API accuracy.'])
