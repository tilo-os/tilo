"""Opt-in worker handoff diagnostics; never presentation qualification."""
from native_delivery import rows
from render_boundaries import _bracket, _interval, _ordered
from runtime_benchmark import distribution

EPILOGUE = ('render_end', 'after_native_worker_log', 'after_outcome_log',
            'after_scene_recycle', 'after_pool_send', 'after_outcome_send',
            'after_completion_notify')
PREVIOUS = EPILOGUE + ('handoff_log_before', 'handoff_log_after')

def _identity(row):
    if any(type(row.get(k)) is not int or row[k] < 0 for k in ('worker_id', 'worker_frame_ix')):
        raise ValueError('Invalid worker handoff identity')
    if 'cpu_frame_id' not in row or (row['cpu_frame_id'] is not None and
            (type(row['cpu_frame_id']) is not int or row['cpu_frame_id'] < 0)):
        raise ValueError('Invalid handoff CPU identity')
    return row['worker_id'], row['worker_frame_ix']

def summarize_worker_handoffs(lines, expected_cpu_ids):
    expected=set(expected_cpu_ids)
    if not expected or len(expected)!=len(expected_cpu_ids) or any(type(i) is not int or i<0 for i in expected):
        raise ValueError('Invalid expected handoff CPU identities')
    handoffs=rows(lines,'qos-native-worker-handoff ')
    if not handoffs:
        return dict(available=False,performance_certified=False,reason='Worker handoff diagnostics unavailable')
    workers={}
    for row in rows(lines,'qos-native-worker '):
        key=_identity(row)
        if key in workers:raise ValueError('Duplicate worker identity')
        workers[key]=row
    seen=set(); by_cpu={}; frames=[]
    for row in handoffs:
        key=_identity(row);cpu=row['cpu_frame_id']
        if key in seen:raise ValueError('Duplicate worker handoff')
        seen.add(key)
        if cpu is not None:
            if cpu in by_cpu:raise ValueError('Duplicate handoff CPU identity')
            by_cpu[cpu]=row
        worker=workers.get(key)
        if worker is None or worker['cpu_frame_id']!=cpu:raise ValueError('Handoff differs from worker identity')
        before,after=(_bracket(row,p) for p in ('before_receive','after_receive'))
        _ordered(before,after)
        if after!=_bracket(worker,'receive'):raise ValueError('Handoff receive differs from worker receive')
        intervals={'receive_call':_interval(before,after)}
        if 'previous_worker_frame_ix' not in row or 'previous_cpu_frame_id' not in row:
            raise ValueError('Missing explicit predecessor identity')
        previous=row['previous_worker_frame_ix']
        if previous is None:
            if key[1]!=0 or row['previous_cpu_frame_id'] is not None:
                raise ValueError('Invalid first handoff predecessor')
            for name in PREVIOUS:
                for end in ('lo','hi'):
                    field='previous_'+name+'_host_'+end+'_s'
                    if field not in row or row[field] is not None:
                        raise ValueError('First handoff requires null predecessor timestamps')
        else:
            if type(previous) is not int or previous<0 or previous!=key[1]-1:
                raise ValueError('Invalid handoff predecessor sequence')
            predecessor=workers.get((key[0],previous))
            if predecessor is None or predecessor['cpu_frame_id']!=row['previous_cpu_frame_id']:
                raise ValueError('Handoff predecessor identity mismatch')
            prev_cpu=row['previous_cpu_frame_id']
            if prev_cpu is not None and (type(prev_cpu) is not int or prev_cpu<0):
                raise ValueError('Invalid predecessor CPU identity')
            points=[_bracket(row,'previous_'+p) for p in EPILOGUE]
            _ordered(*points,before)
            if points[0]!=_bracket(predecessor,'render_end'):
                raise ValueError('Previous render end differs from worker')
            log_before,log_after=(_bracket(row,'previous_'+p) for p in ('handoff_log_before','handoff_log_after'))
            _ordered(_bracket(predecessor,'receive'),log_before,log_after,_bracket(predecessor,'render_start'))
            names=('previous_native_worker_log','previous_outcome_log','previous_scene_recycle',
                   'previous_pool_send','previous_outcome_send','previous_completion_notify')
            intervals.update({name:_interval(a,b) for name,a,b in zip(names,points,points[1:])})
            intervals['previous_epilogue']=_interval(points[0],points[-1])
            intervals['previous_notify_to_receive_entry']=_interval(points[-1],before)
            intervals['previous_handoff_log']=_interval(log_before,log_after)
        if cpu in expected:
            frames.append(dict(cpu_frame_id=cpu,worker_id=key[0],worker_frame_ix=key[1],intervals_ms=intervals))
    if not expected<=by_cpu.keys():raise ValueError('Missing expected worker handoffs')
    # Every received predecessor must itself have a handoff row, including
    # unprofiled/auxiliary frames. The final outgoing epilogue needs no successor.
    for row in handoffs:
        previous=row['previous_worker_frame_ix']
        if previous is not None and (row['worker_id'],previous) not in seen:
            raise ValueError('Missing predecessor handoff')
    frames.sort(key=lambda f:f['cpu_frame_id'])
    names=set().union(*(f['intervals_ms'] for f in frames))
    stages={name:{bound:distribution([f['intervals_ms'][name][index] for f in frames if name in f['intervals_ms']],scale=1)
                  for index,bound in enumerate(('lower','upper'))} for name in sorted(names)}
    return dict(available=True,performance_certified=False,frames=frames,stages=stages,
                limitations=['Intervals include scheduling and synchronization; they do not isolate CPU execution, OS wake latency, or GPU work.',
                             'Previous epilogue and diagnostic emission are observed only when another frame arrives.',
                             'Final outgoing epilogue and final diagnostic emission remain unobserved.',
                             'Instrumentation perturbs the measured path; presentation must be validated separately.'])
