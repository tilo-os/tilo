#!/usr/bin/env python3
"""Capture an isolated native app workload; never infer qualification."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import subprocess
import tempfile
import time
import uuid

from native_delivery import rows, summarize_native_delivery
from presentation_feedback import summarize_presentation
from input_latency import summarize_input_latency
from render_boundaries import summarize_render_boundaries
from main_stages import summarize_main_stages
from worker_handoff import summarize_worker_handoffs
from scratchpad_effects import (validate_scratchpad_typing, validate_scratchpad_scroll,
                               validate_scratchpad_frame_states)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def expand_scenario(scenario):
    """Expand bounded top-level repeat blocks into concrete native inputs."""
    if not isinstance(scenario, dict) or not isinstance(scenario.get('steps'), list):
        raise ValueError('Scenario steps must be a list')
    expanded = []
    for step in scenario['steps']:
        if not isinstance(step, dict):
            raise ValueError('Scenario step must be an object')
        if step.get('kind') != 'repeat':
            expanded.append(step)
            continue
        if set(step) != {'kind', 'count', 'steps'}:
            raise ValueError('Repeat step has unexpected fields')
        count, template = step['count'], step['steps']
        if type(count) is not int or not 1 <= count <= 10_000:
            raise ValueError('Repeat count is outside its bound')
        if not isinstance(template, list) or not template:
            raise ValueError('Repeat template must be a non-empty list')
        if any(not isinstance(item, dict) or item.get('kind') == 'repeat' for item in template):
            raise ValueError('Repeat templates must contain concrete step objects')
        if len(expanded) + count * len(template) > 100_000:
            raise ValueError('Expanded scenario exceeds its step bound')
        for _ in range(count):
            expanded.extend(dict(item) for item in template)
    result = dict(scenario)
    result['steps'] = expanded
    return result


def window_metadata(lines, pointer_passthrough=False):
    windows=rows(lines,'tilo-native-window ')
    if len(windows)!=1:
        raise ValueError('Missing or repeated native window metadata')
    window=windows[0]
    if pointer_passthrough and window.get('pointer_passthrough') is not True:
        raise ValueError('Native window did not confirm physical pointer passthrough')
    if pointer_passthrough and window.get('input_route_isolation') is not True:
        raise ValueError('Native window did not confirm platform input route isolation')
    scale=window.get('scale_factor')
    if type(scale) not in (int,float) or not math.isfinite(scale) or scale<=0:
        raise ValueError('Invalid native window scale')
    if any(type(window.get(key)) is not int or window[key]<=0
           for key in ('physical_width','physical_height')):
        raise ValueError('Invalid native window size')
    return window

def validate_measured_workload(scenario, mode, typing_character='x'):
    """Setup belongs to warmup; every measured input must exercise the workload."""
    warmup = scenario.get('warmup_steps', 0)
    steps = scenario.get('steps', [])
    if type(warmup) is not int or warmup < 0 or warmup >= len(steps):
        raise ValueError('Invalid measured workload boundary')
    for step in steps[warmup:]:
        if mode == 'typing':
            active = (step == {'kind': 'character', 'value': typing_character}
                      or step == {'kind': 'key', 'key': 'backspace'})
        elif mode == 'scroll':
            dy = step.get('dy')
            active = (set(step) == {'kind', 'dx', 'dy'} and step['kind'] == 'wheel'
                      and type(step['dx']) in (int, float) and step['dx'] == 0
                      and type(dy) in (int, float) and math.isfinite(dy) and dy != 0)
        else:
            raise ValueError('Unsupported measured workload mode')
        if not active:
            raise ValueError('Measured input is setup or does not exercise the workload')


def require_running(process):
    status = process.poll()
    if status is not None:
        raise RuntimeError(f'Native app exited early: {status}')


def diagnostics_drained(lines, expected_cpu_ids):
    """Wait for observed optional diagnostics before terminating the producer.

    Legacy binaries emit none. This is a drain gate, not evidence validation;
    strict shape/identity/clock checks still run on the final complete log.
    """
    render_prefixes=('tilo-native-send ', 'tilo-native-worker ', 'tilo-native-present-call ')
    groups=[rows(lines,prefix) for prefix in render_prefixes]
    main=rows(lines,'tilo-native-main-stages ')
    if any(groups) or main:
        if any(not expected_cpu_ids <= {r.get('cpu_frame_id') for r in group} for group in groups):
            return False
    if main and not expected_cpu_ids <= {r.get('cpu_frame_id') for r in main}:
        return False
    handoffs=rows(lines,'tilo-native-worker-handoff ')
    if handoffs and not expected_cpu_ids <= {r.get('cpu_frame_id') for r in handoffs}:
        return False
    # A handoff is emitted before rendering, including auxiliary frames that
    # have no profiled CPU identity. Do not terminate between that row and its
    # corresponding worker outcome just because measured frames have drained.
    worker_keys={(r.get('worker_id'),r.get('worker_frame_ix')) for r in groups[1]}
    if any((r.get('worker_id'),r.get('worker_frame_ix')) not in worker_keys for r in handoffs):
        return False
    return True


def main(app_kind='scratchpad'):
    if app_kind == 'scratchpad':
        app_id = 'dev.tilo.scratchpad'
        fixture_name, open_flag, minimum_lines = 'data', '--data-dir', 1000
        validate_typing, validate_scroll = validate_scratchpad_typing, validate_scratchpad_scroll
        validate_frames = validate_scratchpad_frame_states
    elif app_kind == 'code-editor':
        from code_editor_effects import (validate_code_editor_typing,
            validate_code_editor_scroll, validate_code_editor_frame_states)
        app_id = 'dev.tilo.code-editor'
        fixture_name, open_flag, minimum_lines = 'fixture.rs', None, 2000
        validate_typing, validate_scroll = validate_code_editor_typing, validate_code_editor_scroll
        validate_frames = validate_code_editor_frame_states
    elif app_kind == 'file-explorer':
        from file_explorer_effects import (validate_file_explorer_scroll,
            validate_file_explorer_frame_states)
        app_id = 'dev.tilo.file-explorer'
        fixture_name, open_flag, minimum_lines = 'fixture', None, None
        validate_typing, validate_scroll = None, validate_file_explorer_scroll
        validate_frames = validate_file_explorer_frame_states
    elif app_kind == 'settings':
        from settings_effects import validate_settings_typing, validate_settings_frame_states
        app_id = 'dev.tilo.settings'
        fixture_name, open_flag, minimum_lines = None, None, None
        validate_typing = lambda scenario, lines, _manifest: validate_settings_typing(scenario, lines)
        validate_scroll = None
        validate_frames = validate_settings_frame_states
    else:
        raise ValueError('Unsupported native capture app')
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--fixture-tool',type=Path,required=app_kind != 'settings')
    parser.add_argument('--scenario',type=Path,required=app_kind != 'settings',
                        default=Path(__file__).parent/'scenarios/settings-search.json' if app_kind == 'settings' else None)
    parser.add_argument('--mode',choices=['typing','scroll'],required=app_kind != 'settings',
                        default='typing' if app_kind == 'settings' else None)
    parser.add_argument('--output-root',type=Path,default=Path('target/runtime-proof'))
    parser.add_argument('--no-activate',action='store_true')
    parser.add_argument('--pointer-passthrough',action='store_true')
    args=parser.parse_args()
    if args.pointer_passthrough and not args.no_activate:
        parser.error('--pointer-passthrough requires --no-activate')
    if app_kind == 'settings' and args.fixture_tool is not None:
        parser.error('Settings uses the normal isolated app launch, without a fixture tool')
    binary=args.binary.resolve()
    fixture_tool=args.fixture_tool.resolve() if args.fixture_tool else None
    identity_paths=(binary,fixture_tool) if fixture_tool is not None else (binary,)
    scenario=expand_scenario(json.loads(args.scenario.read_text()))
    args.output_root.mkdir(parents=True,exist_ok=True)
    out=Path(tempfile.mkdtemp(prefix=f'{app_kind}-native-',dir=args.output_root)).resolve()
    print(out,flush=True)
    (out/'scenario.json').write_text(json.dumps(scenario,indent=2))
    hashes={str(path):digest(path) for path in identity_paths}
    metadata=dict(app_kind=app_kind,binary=str(binary),fixture_tool=str(fixture_tool) if fixture_tool is not None else None,hashes_before=hashes,
                  platform=platform.platform(),machine=platform.machine(),
                  window_no_activate=args.no_activate,window_pointer_passthrough=args.pointer_passthrough,window_position='20,60',mode=args.mode)
    report={'performance_certified':False}
    process=None
    exit_status=1
    (out/'metadata.json').write_text(json.dumps(metadata,indent=2))
    try:
        if app_kind == 'file-explorer' and args.mode != 'scroll':
            raise ValueError('File Explorer capture currently requires scroll mode')
        if app_kind == 'settings':
            if args.mode != 'typing':
                raise ValueError('Settings capture currently requires typing mode')
            manifest={'schema': 1, 'app': app_id, 'startup': 'normal_default_network',
                      'readiness': 'first worker presentation plus strict observed state validation',
                      'durable_state_policy': 'effective document heads must remain identical throughout all effects and frames'}
            (out/'fixture.json').write_text(json.dumps(manifest,indent=2))
        else:
            with (out/'fixture.stderr.log').open('w') as error_log:
                result=subprocess.run([str(fixture_tool),str(out/fixture_name)],capture_output=False,
                                      stdout=subprocess.PIPE,stderr=error_log,text=True,timeout=60,check=True)
            (out/'fixture.json').write_text(result.stdout)
            manifest=json.loads(result.stdout)
            if app_kind == 'file-explorer':
                if (type(manifest.get('entry_count')) is not int or manifest['entry_count'] < 1000
                        or type(manifest.get('image_count')) is not int or manifest['image_count'] < 100):
                    raise ValueError('Directory fixture is too small for this workload')
            elif manifest.get('lines',0)<minimum_lines or manifest.get('body_bytes',0)<10000:
                raise ValueError('Fixture is too small for this workload')
        env={key:value for key,value in os.environ.items() if not key.startswith(('TILO_','QUICK_'))}
        env.update(TILO_PROFILE_PRESENTATION='1',TILO_NATIVE_SCENARIO=str(out/'scenario.json'),
                   TILO_PROFILE_OUT=str(out/'frames.jsonl'),TILO_PROFILE_WINDOW_FRONT='1',
                   TILO_PROFILE_WINDOW_NO_ACTIVATE='1' if args.no_activate else '0',
                   TILO_PROFILE_WINDOW_POSITION='20,60',
                   TILO_PROFILE_POINTER_PASSTHROUGH='1' if args.pointer_passthrough else '0',TILO_UI_PRESENT='1',
                   QUICK_APP_ID=app_id,XDG_DATA_HOME=str(out/'xdg'),
                   XDG_CONFIG_HOME=str(out/'config'),XDG_CACHE_HOME=str(out/'cache'),
                   TILO_WORKSPACE_ID=str(uuid.uuid4()))
        with (out/'host-processes.txt').open('w') as host_log:
            subprocess.run(['ps','-Ao','pid,pcpu,comm'],stdout=host_log,check=True)
        with (out/'app.log').open('w') as log:
            launch_args=([str(binary)] if app_kind == 'settings' else
                         [str(binary),*([open_flag] if open_flag else []),str(out/fixture_name)])
            process=subprocess.Popen(launch_args,env=env,stdout=log,stderr=log)
            deadline=time.monotonic()+max(60,len(scenario['steps'])*scenario['interval_us']/1e6+30)
            while True:
                lines=(out/'app.log').read_text().split('\n')[:-1]
                effects=rows(lines,'tilo-native-effect ')
                require_running(process)
                if len(effects)>=len(scenario['steps']):
                    break
                if time.monotonic()>deadline:
                    raise TimeoutError('Native effect delivery timed out')
                time.sleep(.25)
            drain=time.monotonic()+5
            while time.monotonic()<drain:
                require_running(process)
                lines=(out/'app.log').read_text().split('\n')[:-1]
                mapped=rows(lines,'tilo-native-frame ')
                covered={i for row in mapped for i in row['input_ids']}
                callbacks={r['cpu_frame_id'] for r in rows(lines,'tilo-display-frame ')}
                if (covered==set(range(len(scenario['steps'])))
                        and all(r['cpu_frame_id'] in callbacks for r in mapped)
                        and diagnostics_drained(lines,{r['cpu_frame_id'] for r in mapped})):
                    break
                time.sleep(.05)
        require_running(process)
        process.terminate()
        try: process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill();process.wait()
        if process.returncode not in (0, -signal.SIGTERM):
            raise RuntimeError(f'Native app failed during capture shutdown: {process.returncode}')
        lines=(out/'app.log').read_text().splitlines()
        frames=[json.loads(line) for line in (out/'frames.jsonl').read_text().splitlines()]
        metadata['window']=window_metadata(lines,args.pointer_passthrough)
        report.update(summarize_native_delivery(scenario,lines,frames))
        validate=validate_typing if args.mode=='typing' else validate_scroll
        report['verified_effect_steps']=validate(scenario,lines,manifest)
        report['post_draw_state_frames']=validate_frames(scenario,lines)
        validate_measured_workload(scenario,args.mode,typing_character='z' if app_kind == 'settings' else 'x')
        mappings=rows(lines,'tilo-native-frame ')
        measured=[r['cpu_frame_id'] for r in mappings if min(r['input_ids'])>=scenario['warmup_steps']]
        report['input_associated_presentation']=summarize_presentation(lines,measured)
        active=[r['frame'] for r in frames if min(measured)<=r['frame']<=max(measured)]
        report['all_active_presentation']=summarize_presentation(lines,active)
        if not all(report[key]['feedback_complete'] for key in ('input_associated_presentation','all_active_presentation')):
            raise ValueError('Incomplete positive display feedback')
        report['input_to_presentation'] = summarize_input_latency(scenario, lines, frames)
        report['render_boundaries'] = summarize_render_boundaries(lines, measured)
        report['main_stages'] = summarize_main_stages(lines, measured)
        report['worker_handoffs'] = summarize_worker_handoffs(lines, measured)
        if len(measured)<1000:
            raise ValueError('Fewer than 1000 measured active frames')
        if hashes!={str(path):digest(path) for path in identity_paths}:
            raise ValueError('Executable changed during capture')
        exit_status=0
    except (OSError,ValueError,KeyError,TypeError,AttributeError,RuntimeError,TimeoutError,subprocess.SubprocessError) as error:
        report['capture_error']=str(error)
        (out/'failed-report.json').write_text(json.dumps(report,indent=2))
        print(f'Capture failed: {error}',flush=True)
        exit_status=1
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:process.wait(timeout=10)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        metadata['exit_code']=process.returncode if process else None
        after,hash_errors={},{}
        for path in identity_paths:
            try:after[str(path)]=digest(path)
            except OSError as error:hash_errors[str(path)]=str(error)
        metadata['hashes_after']=after
        metadata['final_hash_errors']=hash_errors
        if hash_errors or after!=hashes:
            report.setdefault('capture_error','Executable identity changed or became unreadable')
            report['final_hash_errors']=hash_errors
            (out/'failed-report.json').write_text(json.dumps(report,indent=2))
            exit_status=1
        (out/'metadata.json').write_text(json.dumps(metadata,indent=2))
    if exit_status==0:
        (out/'report.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report,indent=2))
    return exit_status


if __name__=='__main__':
    raise SystemExit(main())
