#!/usr/bin/env python3
"""Bounded screenshot-only agent trials and independently verified usability probes."""
import argparse
import base64
import collections
import getpass
import hashlib
import html
import json
import math
import os
from pathlib import Path
import queue
import struct
import subprocess
import threading
import time
import urllib.error
import urllib.request
import zlib

TASKS = {
    'open-file': {'goal': 'Open Document-000.txt so its contents are displayed.', 'fixture': 'desktop', 'document': 0, 'efficient_steps': 12},
    'rename-workspace': {'goal': 'Rename the current workspace to Research.', 'fixture': 'desktop', 'efficient_steps': 10},
    'scroll-file': {'goal': 'Find Document-060.txt in Files and open it so its contents are displayed.', 'fixture': 'files', 'document': 60, 'efficient_steps': 18},
}
VARIANTS = {'baseline': {}, 'compact': {'viewport': [800, 700]}, 'large-text': {'text_scale': 2}, 'light': {'theme': 'light'}}
NAMED_KEYS = {name.lower(): name for name in ('Enter', 'Return', 'Tab', 'Escape', 'Esc', 'Space', 'Backspace', 'Delete', 'ArrowUp', 'Up', 'ArrowDown', 'Down', 'ArrowLeft', 'Left', 'ArrowRight', 'Right', 'Home', 'End')}

SYSTEM = '''You are operating a desktop application using screenshots. Complete the user's goal through visible mouse, keyboard and wheel input. Coordinates are screenshot pixels. Return one action per turn. You may use modifiers meta, ctrl, alt, shift. Keys must be one printable character or one of Enter, Return, Tab, Escape, Esc, Space, Backspace, Delete, ArrowUp, Up, ArrowDown, Down, ArrowLeft, Left, ArrowRight, Right, Home, End (case insensitive). Wheel positive dy scrolls down. Observe requests a fresh screenshot. Done means you believe the goal is complete; abort means you cannot proceed. Include only a brief visible observation and expected visible feedback, never hidden reasoning. You receive no internal application state.'''
ACTION_SCHEMA = {'type': 'object', 'additionalProperties': False, 'properties': {
    'op': {'type': 'string', 'enum': ['click', 'key', 'type', 'wheel', 'observe', 'done', 'abort']},
    'x': {'type': ['number', 'null']}, 'y': {'type': ['number', 'null']},
    'dx': {'type': ['number', 'null']}, 'dy': {'type': ['number', 'null']},
    'key': {'type': ['string', 'null'], 'description': 'One printable character or supported named key: Enter Return Tab Escape Esc Space Backspace Delete ArrowUp Up ArrowDown Down ArrowLeft Left ArrowRight Right Home End. Names are case insensitive.'}, 'modifiers': {'type': 'array', 'items': {'type': 'string', 'enum': ['meta', 'ctrl', 'alt', 'shift']}},
    'text': {'type': ['string', 'null']}, 'observation': {'type': 'string'}, 'expected_feedback': {'type': 'string'}},
    'required': ['op', 'x', 'y', 'dx', 'dy', 'key', 'modifiers', 'text', 'observation', 'expected_feedback']}


def digest(value):
    return hashlib.sha256(value if isinstance(value, bytes) else json.dumps(value, sort_keys=True).encode()).hexdigest()


def nodes(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from nodes(child)


def rect_of(value):
    if isinstance(value, dict) and all(k in value for k in ('x0', 'y0', 'x1', 'y1')):
        return [value['x0'], value['y0'], value['x1'] - value['x0'], value['y1'] - value['y0']]
    return value if isinstance(value, list) and len(value) == 4 else None


def intersection(a, b):
    if a is None:
        return b
    if b is None:
        return a
    x, y = max(a[0], b[0]), max(a[1], b[1])
    return [x, y, max(0, min(a[0] + a[2], b[0] + b[2]) - x), max(0, min(a[1] + a[3], b[1] + b[3]) - y)]


def visible_semantics(inspection):
    result = []
    viewport = inspection.get('viewport')
    def walk(value, clip, inherited_rect=None):
        if isinstance(value, dict):
            semantic = value.get('semantics') or {}
            if value.get('state', {}).get('hidden', False) or semantic.get('state', {}).get('hidden', False):
                return
            rect = rect_of(value.get('bounds')) or rect_of(value.get('rect')) or inherited_rect
            visible = intersection(rect, clip)
            if visible and (visible[2] <= 0 or visible[3] <= 0):
                return
            if 'role' in value:
                result.append(value)
            child_clip = intersection(clip, rect_of(value.get('child_clip')))
            for key, child in value.items():
                if key not in ('state', 'bounds', 'rect', 'child_clip'):
                    walk(child, child_clip, rect)
        elif isinstance(value, list):
            for child in value:
                walk(child, clip, inherited_rect)
    walk(inspection, [0, 0, *viewport] if viewport else None)
    return result


def png_evidence(path):
    """Validate dimensions, PNG framing, CRCs and nonempty decompressed capture."""
    data = path.read_bytes()
    if len(data) > 24_000_000 or data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('invalid screenshot PNG')
    at, packed, dims, format_info, ended = 8, b'', None, None, False
    while at + 12 <= len(data):
        length = struct.unpack('>I', data[at:at + 4])[0]
        tag, body = data[at + 4:at + 8], data[at + 8:at + 8 + length]
        if at + 12 + length > len(data) or zlib.crc32(tag + body) != struct.unpack('>I', data[at + 8 + length:at + 12 + length])[0]:
            raise ValueError('corrupt screenshot PNG')
        if tag == b'IHDR':
            dims = struct.unpack('>II', body[:8])
            format_info = body[8:]
        if tag == b'IDAT':
            packed += body
        at += length + 12
        if tag == b'IEND':
            ended = True
            break
    if not ended or at != len(data):
        raise ValueError('missing IEND or trailing screenshot data')
    if not dims or not all(1 <= n <= 4096 for n in dims):
        raise ValueError('invalid screenshot dimensions')
    decoder = zlib.decompressobj()
    pixels = decoder.decompress(packed, 80_000_000)
    if not decoder.eof:
        raise ValueError('invalid screenshot compression')
    depth, color, compression, filtering, interlace = format_info
    channels = {2: 3, 6: 4}.get(color)
    if depth != 8 or not channels or compression or filtering or interlace:
        raise ValueError('unsupported screenshot raster format')
    stride = dims[0] * channels
    if len(pixels) != (stride + 1) * dims[1]:
        raise ValueError('invalid screenshot raster size')
    previous = bytearray(stride)
    first_pixel, varied = None, False
    # Reconstruct until a genuinely nonuniform row is found; framing was checked above.
    for row_index in range(dims[1]):
        at = row_index * (stride + 1)
        mode = pixels[at]
        row = bytearray(pixels[at + 1:at + 1 + stride])
        if mode not in range(5):
            raise ValueError('invalid PNG filter')
        for i in range(stride):
            left = row[i - channels] if i >= channels else 0
            up, upper_left = previous[i], previous[i - channels] if i >= channels else 0
            if mode == 1:
                predictor = left
            elif mode == 2:
                predictor = up
            elif mode == 3:
                predictor = (left + up) // 2
            elif mode == 4:
                p = left + up - upper_left
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - upper_left)
                predictor = left if pa <= pb and pa <= pc else up if pb <= pc else upper_left
            else:
                predictor = 0
            row[i] = (row[i] + predictor) & 255
        if first_pixel is None:
            first_pixel = row[:channels]
        if row != first_pixel * dims[0]:
            varied = True
            break
        previous = row
    if not varied:
        raise ValueError('uniform screenshot raster')
    return dims, digest(data)


def evidence(response):
    if response.get('ok') is not True:
        raise ValueError('host rejected command: ' + str(response.get('error'))[:300])
    shot, inspect = Path(response['screenshot']), Path(response['inspection'])
    if not shot.is_absolute() or not inspect.is_absolute():
        raise ValueError('evidence paths must be absolute')
    dims, shot_hash = png_evidence(shot)
    tree = json.loads(inspect.read_text())
    if not visible_semantics(tree) or not isinstance(response.get('state'), dict):
        raise ValueError('missing inspection/state evidence')
    return tree, dims, shot_hash


def oracle(task, response, inspection):
    state = response['state']
    if task == 'rename-workspace':
        return state.get('active_workspace') == 'Research' and 'Research' in response.get('persisted_workspace_names', [])
    number = TASKS[task]['document']
    title = f'Document-{number:03}.txt'
    expected = f'Fixture document {number}\n'
    semantics = visible_semantics(inspection)
    if any(n.get('role') == 'Dialog' or n.get('state', {}).get('modal') for n in semantics):
        return False
    provider = any(t.get('title') == title and t.get('content', {}).get('AppPlaceholder', {}).get('kind') == {'Other': 'Editor'} for t in state.get('active_tabs') or [])
    return provider and any(n.get('name') == title and n.get('value') == expected for n in visible_semantics(inspection))


def validate_action(action, viewport):
    if not isinstance(action, dict) or set(action) != set(ACTION_SCHEMA['required']):
        raise ValueError('action fields do not match schema')
    op = action['op']
    if op not in ACTION_SCHEMA['properties']['op']['enum']:
        raise ValueError('unsupported action')
    for field in ('observation', 'expected_feedback'):
        if not isinstance(action[field], str) or len(action[field]) > 500:
            raise ValueError('action description too long or invalid')
    allowed = {'click': {'x', 'y'}, 'wheel': {'x', 'y', 'dx', 'dy'}, 'key': {'key'}, 'type': {'text'}}.get(op, set())
    for field in ('x', 'y', 'dx', 'dy', 'key', 'text'):
        value = action[field]
        if field not in allowed and value is not None:
            raise ValueError('unused action field must be null')
        if field in allowed:
            if field in ('key', 'text'):
                if not isinstance(value, str) or not value or len(value) > (64 if field == 'key' else 256):
                    raise ValueError('invalid input text/key')
            elif isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
                raise ValueError('coordinates must be finite numbers')
    mods = action['modifiers']
    if not isinstance(mods, list) or any(m not in ('meta', 'ctrl', 'alt', 'shift') for m in mods) or len(mods) != len(set(mods)) or (op != 'key' and mods):
        raise ValueError('invalid modifiers')
    if op in ('click', 'wheel') and not (0 <= action['x'] < viewport[0] and 0 <= action['y'] < viewport[1]):
        raise ValueError('coordinates outside viewport')
    if op == 'wheel' and (abs(action['dx']) > 4096 or abs(action['dy']) > 4096):
        raise ValueError('wheel exceeds bounds')
    result = {'op': op, **{k: action[k] for k in allowed}}
    if op == 'key':
        key = action['key']
        canonical = NAMED_KEYS.get(key.lower())
        if canonical is None and not (len(key) == 1 and key.isprintable()):
            raise ValueError('unsupported named key; use the declared key vocabulary')
        result['key'] = canonical or key
        result['modifiers'] = mods
    if op == 'type' and any(not ch.isprintable() for ch in action['text']):
        raise ValueError('text contains control characters; use named keys for Enter/Tab')
    return result


class Host:
    def __init__(self, binary, scenario, out, timeout=30):
        self.timeout, self.latency, self.frames = timeout, 0, []
        self.log = (out.parent / (out.name + '.stderr.log')).open('w')
        self.process = subprocess.Popen([str(binary), '--interactive', '--scenario', str(scenario), '--out', str(out)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True, bufsize=1)
        self.lines = queue.Queue()
        def reader():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put(None)
        threading.Thread(target=reader, daemon=True).start()
        try:
            self.current = self.receive()
        except (Exception, KeyboardInterrupt):
            self.close()
            raise

    def receive(self):
        start = time.monotonic()
        try:
            line = self.lines.get(timeout=self.timeout)
            if line is None:
                raise ValueError('host exited before response')
            if len(line) > 4_000_000:
                raise ValueError('host response too large')
            response = json.loads(line)
            if not isinstance(response, dict) or response.get('sequence') != len(self.frames):
                raise ValueError('host sequence mismatch')
            counts = response.get('counts')
            if not isinstance(counts, dict) or any(isinstance(n, bool) or not isinstance(n, int) or n < 0 for n in counts.values()):
                raise ValueError('invalid host input counts')
            evidence(response)
            self.current = response
            self.frames.append(response)
            return response
        except queue.Empty:
            raise TimeoutError('host response deadline exceeded') from None
        finally:
            self.latency += time.monotonic() - start

    def send(self, command):
        self.process.stdin.write(json.dumps(command, allow_nan=False) + '\n')
        self.process.stdin.flush()
        return self.receive()

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.log.close()


class API:
    def __init__(self, key, model, max_tokens, timeout):
        self.key, self.model, self.max_tokens, self.timeout = key, model, max_tokens, timeout

    def action(self, goal, screenshot, history, remaining):
        image = base64.b64encode(Path(screenshot).read_bytes()).decode()
        # Only pixels, plain task text and the agent's own previous actions cross this boundary.
        body = {'model': self.model, 'store': False, 'max_output_tokens': min(self.max_tokens, remaining), 'reasoning': {'effort': 'low'},
                'input': [{'role': 'system', 'content': SYSTEM}, {'role': 'user', 'content': [
                    {'type': 'input_text', 'text': goal + '\nYour previous actions: ' + json.dumps(history)},
                    {'type': 'input_image', 'image_url': 'data:image/png;base64,' + image, 'detail': 'original'}]}],
                'text': {'format': {'type': 'json_schema', 'name': 'desktop_action', 'strict': True, 'schema': ACTION_SCHEMA}}}
        request = urllib.request.Request('https://api.openai.com/v1/responses', data=json.dumps(body).encode(), headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + self.key})
        start = time.monotonic()
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as stream:
                response = json.load(stream)
        except urllib.error.HTTPError as error:
            # Never log the body, headers, request object or credentials; never retry charged failures.
            raise RuntimeError(f'OpenAI HTTP {error.code}; no automatic retry') from None
        except (urllib.error.URLError, TimeoutError):
            raise RuntimeError('OpenAI transport failure; no automatic retry') from None
        elapsed = time.monotonic() - start
        usage = response.get('usage', {})
        text = ''.join(part.get('text', '') for item in response.get('output', []) for part in item.get('content', []) if part.get('type') == 'output_text')
        return text, usage, elapsed, response.get('status'), response.get('model')


def assess(task, response, inspection, actions, hashes, max_steps, errors=()):
    verified = oracle(task, response, inspection) and not errors
    noops = sum(a == b for a, b in zip(hashes, hashes[1:]))
    repeats = sum(n - 1 for n in collections.Counter(hashes).values() if n > 1)
    efficient = verified and len(actions) <= min(max_steps, TASKS[task]['efficient_steps']) and noops <= 2 and repeats <= 3
    return {'verified_success': verified, 'efficient_success': efficient, 'input_steps': len(actions), 'noops': noops, 'repeated_states': repeats,
            'counts': dict(collections.Counter(a['op'] for a in actions)), 'errors': list(errors)}


def agent_attempt(binary, out, task, variant, api, limits):
    out.mkdir(parents=True)
    scenario = {'name': task, 'goal': TASKS[task]['goal'], 'fixture': TASKS[task]['fixture'], 'viewport': [1280, 800], 'budget': {'clicks': 100, 'keys': 1000}, 'steps': [{'op': 'checkpoint', 'name': 'observation', 'checks': [{'kind': 'state', 'pointer': '/active_workspace', 'equals': 'Home'}]}], **VARIANTS[variant]}
    config = out / 'scenario.json'
    config.write_text(json.dumps(scenario))
    host, actions, history, hashes, calls, errors = None, [], [], [], [], []
    claim, tokens, api_latency, final = None, 0, 0, None
    exhausted, interrupted = False, False
    def journal(event):
        with (out / 'journal.jsonl').open('a') as stream:
            stream.write(json.dumps(event, allow_nan=False) + '\n')
            stream.flush()
            os.fsync(stream.fileno())
    started = time.monotonic()
    try:
        host = Host(binary, config, out / 'host')
        hashes.append(evidence(host.current)[2])
        for step in range(limits['steps']):
            if time.monotonic() - started >= limits['seconds'] or tokens >= limits['tokens']:
                errors.append('attempt deadline/token budget reached')
                break
            api.timeout = min(limits['api_seconds'], max(1, limits['seconds'] - (time.monotonic() - started)))
            call_started = time.monotonic()
            try:
                raw, usage, latency, status, resolved_model = api.action(TASKS[task]['goal'], host.current['screenshot'], history, limits['tokens'] - tokens)
            except (Exception, KeyboardInterrupt) as call_error:
                latency = time.monotonic() - call_started
                api_latency += latency
                calls.append({'usage': None, 'api_seconds': latency, 'status': 'interrupted_usage_unknown' if isinstance(call_error, KeyboardInterrupt) else 'request_failed_usage_unknown', 'model': api.model})
                journal({'event': 'api_call', **calls[-1]})
                raise
            api_latency += latency
            calls.append({'usage': usage, 'api_seconds': latency, 'status': status, 'model': resolved_model})
            journal({'event': 'api_call', **calls[-1]})
            if not isinstance(usage.get('total_tokens'), int) or usage['total_tokens'] < 0:
                raise ValueError('API response missing valid token accounting')
            tokens += usage['total_tokens']
            if tokens > limits['tokens']:
                raise ValueError('API token budget exceeded')
            if status != 'completed':
                raise ValueError('API response did not complete')
            action = json.loads(raw)
            command = validate_action(action, evidence(host.current)[1])
            history.append(action)
            journal({'event': 'validated_action', 'decision': action, 'command': command})
            if command['op'] in ('done', 'abort'):
                claim = command['op']
                break
            actions.append(command)
            journal({'event': 'input_dispatched', 'command': command})
            response = host.send(command)
            journal({'event': 'input_completed', 'command': command, 'sequence': response.get('sequence'), 'screenshot': response['screenshot'], 'inspection': response['inspection']})
            hashes.append(evidence(response)[2])
        else:
            exhausted = True
        final = host.send({'op': 'finish'})
        tree = evidence(final)[0]
        result = assess(task, final, tree, actions, hashes, limits['steps'], errors)
        if exhausted:
            result['efficient_success'] = False
            result['errors'].append('step/API-call budget exhausted; final outcome still independently assessed')
        if claim == 'done' and not result['verified_success']:
            result['errors'].append('false completion: independent oracle failed')
        if claim == 'abort':
            result['efficient_success'] = False
    except (Exception, KeyboardInterrupt) as error:
        interrupted = isinstance(error, KeyboardInterrupt)
        errors.append('Interrupted by user; partial evidence retained' if interrupted else f'{type(error).__name__}: {error}')
        result = {'verified_success': False, 'efficient_success': False, 'errors': errors, 'input_steps': len(actions), 'counts': dict(collections.Counter(a['op'] for a in actions))}
    finally:
        if host:
            host.close()
    result.update(interrupted=interrupted, kind='agent', task=task, variant=variant, claim=claim, actions=actions, calls=calls, tokens=tokens, api_seconds=api_latency,
                  ui_capture_seconds=host.latency if host else 0, wall_seconds=time.monotonic() - started, model=api.model,
                  decisions=history, prompt_hash=digest({'system': SYSTEM, 'goal': TASKS[task]['goal'], 'schema': ACTION_SCHEMA}), limits=limits,
                  frames=host.frames if host else [], directory=str(out))
    result['classification'] = 'success' if result['verified_success'] else ('execution_error' if errors else 'agent_task_failure')
    (out / 'result.json').write_text(json.dumps(result, indent=2))
    (out / 'replay.jsonl').write_text(''.join(json.dumps(a) + '\n' for a in actions + [{'op': 'finish'}]))
    return result


def geometry_audit(inspection):
    viewport = inspection['viewport']
    controls, failures, clipped, partial = [], [], 0, []
    def walk(node, ancestors=(), inherited_clip=None):
        nonlocal clipped
        if not isinstance(node, dict):
            return
        semantic = node.get('semantics') or {}
        if semantic.get('state', {}).get('hidden', False):
            return
        rect = node.get('rect')
        path = node.get('path', '')
        actionable = node.get('focusable') or semantic.get('role') in ('Button', 'TextInput', 'CheckBox', 'RadioButton', 'MenuItem', 'Tab', 'Link', 'Slider', 'TextField', 'ListItem', 'TreeItem')
        if actionable and not semantic.get('state', {}).get('hidden', False):
            label = semantic.get('name') or ''
            valid = isinstance(rect, list) and len(rect) == 4 and all(isinstance(x, (int, float)) and math.isfinite(x) for x in rect)
            if valid:
                x, y, w, h = rect
                clip = inherited_clip or [0, 0, *viewport]
                cx, cy, cw, ch = clip
                if x + w <= cx or y + h <= cy or x >= cx + cw or y >= cy + ch:
                    clipped += 1
                    actionable = False
                else:
                    visible_rect = intersection(rect, clip)
                    if visible_rect != rect:
                        partial.append({'target': label, 'rect': rect, 'visible_rect': visible_rect, 'reason': 'Partially clipped control excluded from full-target geometry; scrolling/clipping can be intentional.'})
                        actionable = False
                    if actionable and (w < 24 or h < 24):
                        failures.append({'property': 'target_size_24', 'target': label, 'rect': rect})
            elif actionable:
                failures.append({'property': 'finite_bounds', 'target': label})
            if actionable:
                if not label.strip():
                    failures.append({'property': 'accessible_label', 'target': path})
                controls.append({'path': path, 'ancestors': ancestors, 'label': label, 'rect': rect if valid else None})
        own_clip = intersection(inherited_clip, rect_of(node.get('child_clip')))
        for index, projected in enumerate(semantic.get('children', [])):
            projected_rect = rect_of(projected.get('bounds'))
            if projected_rect:
                walk({'path': path + '/semantic-' + str(index), 'rect': projected_rect, 'semantics': projected, 'focusable': projected.get('state', {}).get('focusable', False)}, ancestors + (path,), own_clip)
        for child in node.get('children', []):
            walk(child, ancestors + (path,), own_clip)
    walk(inspection.get('root', {}), inherited_clip=[0, 0, *viewport])
    peer_pairs = 0
    for i, a in enumerate(controls):
        for b in controls[i + 1:]:
            if not a['rect'] or not b['rect'] or a['path'] in b['ancestors'] or b['path'] in a['ancestors']:
                continue
            peer_pairs += 1
            ax, ay, aw, ah = a['rect']; bx, by, bw, bh = b['rect']
            # Signed gaps between the two boxes on each axis: negative = they
            # overlap on that axis, zero = they share an edge.
            hgap = max(bx - (ax + aw), ax - (bx + bw))
            vgap = max(by - (ay + ah), ay - (by + bh))
            if min(ax + aw, bx + bw) - max(ax, bx) > 1 and min(ay + ah, by + bh) - max(ay, by) > 1:
                failures.append({'property': 'semantic_bounds_overlap_unresolved', 'targets': [a['label'], b['label']]})
            elif hgap <= 0 and vgap <= 0:
                # Touching (a shared edge) or a sub-pixel overlap: HIG §7 wants a
                # token-step gap between neighbouring elements.
                failures.append({'property': 'touching_peers', 'targets': [a['label'], b['label']], 'gap': [round(hgap, 2), round(vgap, 2)]})
    return {'measured_controls': len(controls), 'fully_clipped_controls': clipped, 'partially_clipped_controls': partial, 'peer_pairs': peer_pairs, 'failures': failures,
            'passed': bool(controls) and not failures, 'scope': 'Visible widget and bounded projected actionable semantics; ancestor overlaps excluded. Clipped controls explicitly excluded. Overlap and touching are semantic-bound candidates requiring pixel/hit confirmation, not proof of painted overlap; segments of one control that deliberately share an edge report as touching. No inference about unlabeled custom gesture areas.'}


def deterministic_attempt(binary, out, variant):
    out.mkdir(parents=True)
    config = out / 'scenario.json'
    config.write_text(json.dumps({'name': 'Deterministic usability', 'goal': 'Probe keyboard, recovery and feedback', 'fixture': 'desktop', 'viewport': [1280, 800], 'budget': {'clicks': 100, 'keys': 1000}, 'steps': [{'op': 'checkpoint', 'name': 'observation', 'checks': [{'kind': 'state', 'pointer': '/active_workspace', 'equals': 'Home'}]}], **VARIANTS[variant]}))
    host, properties, actions, audits = None, [], [], []
    def check(name, passed, detail):
        properties.append({'property': name, 'passed': bool(passed), 'detail': detail})
    def send(command):
        actions.append(command)
        return host.send(command)
    def names(response):
        return [n.get('name', '') for n in visible_semantics(evidence(response)[0])]
    try:
        host = Host(binary, config, out / 'host')
        initial = host.current
        audits.append(geometry_audit(evidence(initial)[0]))
        palette = send({'op': 'key', 'key': 'Space', 'modifiers': ['meta']})
        check('keyboard_entry', 'Command palette' in names(palette), 'Command-Meta K opens a visible command palette')
        check('visible_feedback', evidence(initial)[2] != evidence(palette)[2] and 'Command palette' in names(palette), 'Pixels change and a palette appears after input')
        send({'op': 'type', 'text': 'no-such-app-zzzyyy'})
        cancelled = send({'op': 'key', 'key': 'Escape', 'modifiers': []})
        check('escape_cancellation', 'Command palette' not in names(cancelled), 'Escape closes palette after an incorrect query')
        check('state_preservation', cancelled['state'] == initial['state'], 'Workspace and tab state exactly preserved after cancelled search')
        send({'op': 'key', 'key': 'Space', 'modifiers': ['meta']})
        wrong = send({'op': 'type', 'text': 'no-such-app-zzzyyy'})
        check('wrong_search_feedback', any('No results' in n or 'No matching' in n or 'Nothing' in n for n in names(wrong)), 'Empty results has visible semantic feedback')
        send({'op': 'key', 'key': 'a', 'modifiers': ['ctrl']})
        correct = send({'op': 'type', 'text': 'Files'})
        opened = send({'op': 'key', 'key': 'Enter', 'modifiers': []})
        opened = send({'op': 'observe'})
        if 'Files' not in (opened['state'].get('tab_titles') or []):
            opened = send({'op': 'observe'})
        check('wrong_search_recovery', 'Files' in (opened['state'].get('tab_titles') or []) and evidence(wrong)[2] != evidence(correct)[2], 'Replace wrong query and open Files')
        check('keyboard_task', any(t.get('content', {}).get('AppPlaceholder', {}).get('kind') == 'FileExplorer' for t in opened['state'].get('active_tabs') or []), 'Files provider reached through keyboard only')
        audits.append(geometry_audit(evidence(opened)[0]))
        # Deterministic route may use inspection for locating a visible row; agent route cannot.
        tree = evidence(opened)[0]
        targets = [rect_of(n.get('bounds')) for n in visible_semantics(tree) if n.get('name') == 'Document-000.txt' and rect_of(n.get('bounds'))]
        if targets:
            x, y, w, h = targets[0]
            send({'op': 'click', 'x': x + w / 2, 'y': y + h / 2})
            document = send({'op': 'key', 'key': 'Enter', 'modifiers': []})
            for _ in range(3):
                if oracle('open-file', document, evidence(document)[0]):
                    break
                document = send({'op': 'observe'})
            check('exact_document_outcome', oracle('open-file', document, evidence(document)[0]), 'Exact document body and Editor identity independently verified')
        else:
            check('exact_document_outcome', False, 'Visible document row unavailable to deterministic target lookup')
        final = host.send({'op': 'finish'})
        check('hit_geometry', all(a['passed'] for a in audits), {'audits': audits})
        error = None
    except Exception as exc:
        error = f'{type(exc).__name__}: {exc}'
        check('execution', False, error)
    finally:
        if host:
            host.close()
    result = {'kind': 'deterministic', 'variant': variant, 'properties': properties, 'verified_success': all(p['passed'] for p in properties),
              'classification': 'execution_error' if error else ('success' if all(p['passed'] for p in properties) else 'product_property_failure'),
              'actions': actions, 'frames': host.frames if host else [], 'ui_capture_seconds': host.latency if host else 0, 'directory': str(out)}
    (out / 'result.json').write_text(json.dumps(result, indent=2))
    (out / 'replay.jsonl').write_text(''.join(json.dumps(a) + '\n' for a in actions + [{'op': 'finish'}]))
    return result


EXPECTED_PROPERTIES = ('keyboard_entry', 'visible_feedback', 'escape_cancellation', 'state_preservation', 'wrong_search_feedback', 'wrong_search_recovery', 'keyboard_task', 'exact_document_outcome', 'hit_geometry')


def write_report(out, results, prior=None, expected_attempts=0):
    agents = [r for r in results if r['kind'] == 'agent']
    properties = collections.defaultdict(lambda: {'passed': 0, 'measured': 0, 'planned': 0, 'skipped': 0})
    deterministic_runs = [r for r in results if r['kind'] == 'deterministic']
    for name in EXPECTED_PROPERTIES:
        properties[name]['planned'] = len(deterministic_runs)
        properties[name]['skipped'] = sum(not any(p['property'] == name for p in r.get('properties', [])) for r in deterministic_runs)
    for r in results:
        for p in r.get('properties', []):
            properties[p['property']]['measured'] += 1
            properties[p['property']]['passed'] += int(p['passed'])
    gaps = ['Subjective ease, delight and human comprehension are unmeasured.', 'Keyboard probes cover a specific route; assistive technology operation and all-control keyboard reachability remain unmeasured.', 'Geometry covers captured actionable semantics; contrast/spacing/motion remain separate prior matrix measurements.', 'Token ceilings stop subsequent calls and invalidate overshooting responses; the final accepted request can exceed remaining tokens because image input usage is reported after processing. Failed requests may have unknown chargeable usage.']
    if len(agents) < expected_attempts:
        gaps.append(f'Live agent campaign incomplete: {len(agents)}/{expected_attempts} attempts recorded.')
    summary = {'schema_version': 1, 'agent_denominator': len(agents), 'planned_agent_attempts': expected_attempts,
               'verified_task_successes': sum(r['verified_success'] for r in agents), 'efficient_task_successes': sum(r['efficient_success'] for r in agents),
               'properties': dict(properties), 'runs': results, 'prior_matrix': prior, 'limitations': gaps,
               'total_tokens': sum(r.get('tokens', 0) for r in agents)}
    (out / 'summary.json').write_text(json.dumps(summary, indent=2))
    sections = []
    for r in results:
        title = f"{r['kind']} {r.get('task', 'usability probes')} / {r['variant']}"
        frames = []
        for i, frame in enumerate(r.get('frames', [])):
            relative = os.path.relpath(frame['screenshot'], out)
            command = 'initial' if i == 0 else (r['actions'][i - 1] if i - 1 < len(r['actions']) else 'finish')
            frames.append(f'<details><summary>Frame {i}: {html.escape(json.dumps(command))}</summary><img loading="lazy" src="{html.escape(relative, quote=True)}" alt="Application screenshot {i}"></details>')
        detail = {k: v for k, v in r.items() if k not in ('frames', 'actions', 'directory')}
        result_link = os.path.relpath(Path(r['directory']) / 'result.json', out)
        replay_link = os.path.relpath(Path(r['directory']) / 'replay.jsonl', out)
        sections.append(f'<section><h2>{html.escape(title)}</h2><p><b>{html.escape(r["classification"])}</b> · <a href="{result_link}">JSON evidence</a> · <a href="{replay_link}">Replay inputs</a></p><details><summary>Measurements and failures</summary><pre>{html.escape(json.dumps(detail, indent=2))}</pre></details>{"".join(frames)}</section>')
    rows = ''.join(f'<tr><td>{html.escape(k)}</td><td>{v["passed"]}/{v["planned"]} planned · {v["measured"]} measured · {v["skipped"]} skipped</td></tr>' for k, v in properties.items())
    document = '<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Usability evidence</title><style>body{font:16px/1.5 system-ui;background:#f5f6f8;color:#202530}main{max-width:1100px;margin:auto;padding:24px}section{background:white;border:1px solid #cbd2dc;border-radius:10px;padding:20px;margin:20px 0}img{max-width:100%;height:auto}pre{white-space:pre-wrap;overflow-wrap:anywhere}td,th{padding:8px;text-align:left}summary{cursor:pointer;margin:8px 0}a{color:#174c91}</style><main><h1>Desktop usability evidence</h1>'
    document += f'<p>Verified task success: {summary["verified_task_successes"]}/{len(agents)}. Efficient success: {summary["efficient_task_successes"]}/{len(agents)}. Tokens: {summary["total_tokens"]}.</p><p>Task completion is checked privately against exact persisted state or document text and provider identity. Agent self-reports do not establish success. These measurements do not certify good UX.</p><table><tr><th>Deterministic property</th><th>Passed / planned; measured and skipped</th></tr>{rows}</table>'
    if prior:
        document += '<p>Prior matrix: <a href="' + html.escape(os.path.relpath(prior, out), quote=True) + '">Separate regression evidence</a></p>'
    document += '<h2>Coverage limits</h2><ul>' + ''.join('<li>' + html.escape(g) + '</li>' for g in gaps) + '</ul>' + ''.join(sections) + '</main></html>'
    (out / 'index.html').write_text(document)
    return summary


def replay(binary, source, out):
    out.mkdir(parents=True)
    config = out / 'scenario.json'
    config.write_text((source / 'scenario.json').read_text())
    host = Host(binary, config, out / 'host')
    try:
        for line in (source / 'replay.jsonl').read_text().splitlines():
            host.send(json.loads(line))
        (out / 'replay-observations.json').write_text(json.dumps(host.frames, indent=2))
    finally:
        host.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/ux-bench'))
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--mode', choices=['deterministic', 'agent', 'all', 'replay'], default='deterministic')
    parser.add_argument('--variants', nargs='+', choices=list(VARIANTS), default=['baseline', 'compact-large-text'])
    parser.add_argument('--tasks', nargs='+', choices=list(TASKS), default=list(TASKS))
    parser.add_argument('--trials', type=int, default=2)
    parser.add_argument('--steps', type=int, default=24)
    parser.add_argument('--tokens', type=int, default=60000, help='Per-attempt total input plus output token ceiling')
    parser.add_argument('--total-tokens', type=int, default=500000)
    parser.add_argument('--seconds', type=int, default=600)
    parser.add_argument('--api-seconds', type=int, default=90)
    parser.add_argument('--max-output-tokens', type=int, default=2000)
    parser.add_argument('--model', default='gpt-6-astra')
    parser.add_argument('--key-prompt', action='store_true', help='Read API key from non-echoing prompt even if environment is set')
    parser.add_argument('--prior-matrix', type=Path)
    parser.add_argument('--replay-source', type=Path)
    args = parser.parse_args()
    for name in ('trials', 'steps', 'tokens', 'total_tokens', 'seconds', 'api_seconds', 'max_output_tokens'):
        if not 1 <= getattr(args, name) <= {'trials': 20, 'steps': 100, 'seconds': 3600, 'api_seconds': 180}.get(name, 2_000_000):
            parser.error(f'{name} outside finite supported bounds')
    binary, out = args.binary.resolve(), args.out.resolve()
    if out.exists():
        parser.error('choose a new output directory to preserve evidence')
    if not binary.is_file():
        parser.error('build ux-bench first')
    if args.mode == 'replay':
        if not args.replay_source:
            parser.error('--replay-source required')
        replay(binary, args.replay_source.resolve(), out)
        return 0
    out.mkdir(parents=True)
    results = []
    expected = len(args.tasks) * len(args.variants) * args.trials if args.mode in ('all', 'agent') else 0
    prior = str(args.prior_matrix.resolve()) if args.prior_matrix else None
    if args.mode in ('all', 'deterministic'):
        for variant in args.variants:
            result = deterministic_attempt(binary, out / ('deterministic-' + variant), variant)
            results.append(result)
            print(result['classification'], variant, flush=True)
            write_report(out, results, prior, expected)
    if args.mode in ('all', 'agent'):
        key = (None if args.key_prompt else os.environ.get('OPENAI_API_KEY')) or getpass.getpass('OpenAI API key (not stored): ')
        api = API(key, args.model, args.max_output_tokens, args.api_seconds)
        for trial in range(args.trials):
            for variant in args.variants:
                for task in args.tasks:
                    remaining = args.total_tokens - sum(r.get('tokens', 0) for r in results)
                    if remaining <= 0:
                        write_report(out, results, prior, expected)
                        return 1
                    limits = {'steps': args.steps, 'tokens': min(args.tokens, remaining), 'seconds': args.seconds, 'api_seconds': args.api_seconds}
                    result = agent_attempt(binary, out / f'agent-{trial + 1}-{variant}-{task}', task, variant, api, limits)
                    results.append(result)
                    print(result['classification'], task, variant, f'trial={trial + 1}', f'tokens={result["tokens"]}', flush=True)
                    write_report(out, results, prior, expected)
                    if result.get('interrupted'):
                        print('Campaign interrupted; partial evidence saved.', flush=True)
                        return 130
    summary = write_report(out, results, prior, expected)
    print(out / 'index.html')
    return 0 if all(r['verified_success'] and (r['kind'] != 'agent' or r['efficient_success']) for r in results) and len([r for r in results if r['kind'] == 'agent']) == expected else 1


VARIANTS['compact-large-text'] = {'viewport': [800, 700], 'text_scale': 2}
if __name__ == '__main__':
    raise SystemExit(main())
