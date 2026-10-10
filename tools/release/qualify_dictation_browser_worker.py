#!/usr/bin/env python3
"""Concurrent public-audio worker/private-browser qualification; never live capture.

Uses the existing verified image review and guarded CEF fixture. Browser and
recognizer run concurrently with separate bounded scopes, private profiles and
no external network. This is not a compositor/microphone acceptance test.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import selectors
import subprocess
import sys
import time

PROJECT = Path(__file__).resolve().parents[2]
BROWSER = r'''
import json,sys
sys.path.insert(0,sys.argv[1]);sys.argv=['fixture',sys.argv[2]]
from guarded_text_input import GuardedTextInput
GuardedTextInput.setUpClass()
t=GuardedTextInput('test_native_email_number_replace_cancel_repeat_and_history');t.setUp()
lease=None;target=None;retired=False
try:
 print(json.dumps({'event':'ready'}),flush=True)
 for line in sys.stdin:
  if len(line)>8192:raise RuntimeError('bounded synthetic request required')
  command=json.loads(line);action=command['action']
  if action=='begin':
   target=command['target'];assert target in ['first','notes'];retired=False
   t.main_world('('+target+'.focus(),'+target+'.setSelectionRange(6,10),true)')
   lease=t.prepare();assert t.request_input('start',lease=lease)['ok']
  elif action=='draft':
   assert t.request_input('check',lease=lease)['ok'] is (not retired)
   assert t.main_world(target+'.value')=='alpha beta gamma'
  elif action=='focus':
   t.main_world('(second.focus(),'+target+'.focus(),'+target+'.setSelectionRange(6,10),true)')
   assert not t.request_input('check',lease=lease)['ok'];retired=True
  elif action=='cancel':
   assert t.request_input('cancel',lease=lease)['ok'];retired=True
   assert t.main_world(target+'.value')=='alpha beta gamma'
  elif action=='final':
   text=command['text'];assert isinstance(text,str) and 0<len(text.encode())<=4096
   result=t.request_input('commit',lease=lease,text=text)
   assert result['ok'] is (not retired),result
   assert t.main_world('second.value')=='second'
   if retired:assert t.main_world(target+'.value')=='alpha beta gamma'
   else:
    assert t.main_world(target+'.value')=='alpha '+text+' gamma'
    assert not t.request_input('commit',lease=lease,text=text)['ok']
    assert t.main_world("document.execCommand('undo')")
    assert t.main_world(target+'.value')=='alpha beta gamma'
    assert t.main_world("document.execCommand('redo')")
    assert t.main_world(target+'.value')=='alpha '+text+' gamma'
    assert t.main_world("document.execCommand('undo')")
  else:raise RuntimeError('unknown synthetic action')
  print(json.dumps({'event':action,'retired':retired}),flush=True)
finally:
 t.doCleanups();GuardedTextInput.tearDownClass()
'''


def read_line(process, timeout):
    buffer = getattr(process, '_qualification_lines', bytearray())
    process._qualification_lines = buffer
    deadline = time.monotonic() + timeout
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        while b'\n' not in buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise RuntimeError('owned process did not acknowledge its synthetic request')
            data = os.read(process.stdout.fileno(), 65536)
            if not data or len(buffer) + len(data) > 1024 * 1024:
                raise RuntimeError('missing or unbounded owned process reply')
            buffer.extend(data)
    line, _, tail = buffer.partition(b'\n')
    buffer[:] = tail
    return line.decode() + '\n'


def browser_request(process, command):
    process.stdin.write(json.dumps(command) + '\n')
    process.stdin.flush()
    response = json.loads(read_line(process, 15))
    if response['event'] != command['action']:
        raise RuntimeError('wrong private browser acknowledgement')
    return response


def validate_browser(host, receipt_path):
    receipt = json.loads(receipt_path.read_text())
    if (receipt['exit_code'] != 0 or host.is_symlink()
            or hashlib.sha256(host.read_bytes()).hexdigest() != receipt['host_sha256']):
        raise RuntimeError('compiled native browser receipt differs')
    for name in ['apps/browser/chromium/src/host.cc', 'apps/browser/chromium/src/page_automation.js',
                 'apps/browser/chromium/src/page_automation.h']:
        if hashlib.sha256((PROJECT / name).read_bytes()).hexdigest() != receipt['sources'][name]:
            raise RuntimeError('native editing source differs from compiled host')
    return receipt


def worker_run(browser, arguments, index, mode):
    unit = f'tilo-dictation-browser-worker-{os.getpid()}-{index}'
    command = ['systemd-run', '--user', '--wait', '--pipe', '--collect', '--unit=' + unit,
               '--property=MemoryMax=1280M', '--property=MemoryHigh=1152M',
               '--property=MemorySwapMax=0', '--property=CPUQuota=200%',
               '--property=CPUWeight=20', '--property=TasksMax=64', '--property=RuntimeMaxSec=240',
               sys.executable, str(PROJECT / 'tools/release/qualify_whistle_image_scope.py'),
               '--review-dir', str(arguments.review_dir), '--manifest', str(arguments.manifest),
               '--case', 'fr_fr-0', '--stream-replies']
    # Every execution needs a fresh result name; no old transcript can satisfy it.
    result = arguments.review_dir / 'fr_fr-0-complete-1-result.json'
    if result.exists():
        raise RuntimeError('fresh image review result required')
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, bufsize=1)
    events, done, final, cancelled, recognition = [], 0, None, False, None
    started = time.monotonic()
    try:
        while True:
            line = read_line(process, 180)
            if line.startswith('WHISTLE_AOT_WORKER_REPLY '):
                event = json.loads(line.split(' ', 1)[1])
                events.append(event)
                if event['kind'] == 'draft':
                    browser_request(browser, {'action': 'draft'})
                    if mode == 'focus' and not any(row.get('focus_retired') for row in events):
                        browser_request(browser, {'action': 'focus'})
                        event['focus_retired'] = True
                    if mode == 'cancel':
                        browser_request(browser, {'action': 'cancel'})
                        subprocess.run(['systemctl', '--user', 'stop', unit], check=True, timeout=15)
                        cancelled = True
                        break
                if event['kind'] == 'done':
                    done += 1
                    final = event['text']
                    browser_request(browser, {'action': 'final', 'text': final})
            elif line.startswith('WHISTLE_IMAGE_RESULT '):
                recognition = json.loads(line.split(' ', 1)[1])
                break
        status = process.wait(timeout=15)
        stderr = process.stderr.read(65536)
        if cancelled:
            trailing = bytes(getattr(process, '_qualification_lines', b'')).decode()
            trailing += process.stdout.read(1024 * 1024 + 1)
            if len(trailing) > 1024 * 1024:
                raise RuntimeError('unbounded cancellation output')
            for line in trailing.splitlines():
                if line.startswith('WHISTLE_AOT_WORKER_REPLY '):
                    event = json.loads(line.split(' ', 1)[1])
                    events.append(event)
                    if event['kind'] == 'done':
                        raise RuntimeError('cancelled worker published a trailing final')
            if done or not any(row['kind'] == 'draft' for row in events):
                raise RuntimeError('cancellation did not observe recognition before retiring it')
        elif (status or done != 1 or not recognition or recognition['guarded']):
            raise RuntimeError('actual worker did not complete exactly once: ' + stderr)
        if not events or events[0]['kind'] != 'ready':
            raise RuntimeError('production worker readiness was not observed')
        return dict(mode=mode, replies=events, final=final, scope_exit=status,
                    decoder_exit=None if cancelled else recognition['exit'],
                    cancelled=cancelled, seconds=time.monotonic() - started,
                    recognition=recognition, stderr=stderr)
    except Exception as error:
        if process.poll() is None:
            subprocess.run(['systemctl', '--user', 'stop', unit], check=True, timeout=15)
        process.wait(timeout=15)
        diagnostic = process.stderr.read(65536)
        (arguments.review_dir / f'concurrent-{index}-failure.log').write_text(diagnostic)
        raise RuntimeError('owned recognition scope failed: ' + diagnostic) from error
    finally:
        if process.poll() is None:
            subprocess.run(['systemctl', '--user', 'stop', unit], check=True, timeout=15)
        process.wait(timeout=15)
        process.stdout.close()
        process.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--review-dir', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--browser-host', type=Path, required=True)
    parser.add_argument('--browser-receipt', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--container-image', required=True,
                        help='Immutable OCI image ID or repository digest for the reviewed browser environment')
    args = parser.parse_args()
    if not re.fullmatch(r'(?:[a-z0-9][a-z0-9./:_-]*@)?sha256:[0-9a-f]{64}', args.container_image):
        parser.error('an immutable container image ID or repository digest is required')
    if os.getuid() == 0 or args.output.exists():
        parser.error('unprivileged owned preparation and fresh output required')
    receipt = validate_browser(args.browser_host, args.browser_receipt)
    base = PROJECT.parent
    if not all(path.resolve().is_relative_to(base) for path in
               [args.review_dir, args.manifest, args.browser_host, args.browser_receipt, args.output]):
        parser.error('all fixtures must remain in the owned remote workspace')
    command = ['podman', 'run', '--rm', '-i', '--network=none', '--userns=keep-id',
               '--user', f'{os.getuid()}:{os.getgid()}', '--memory=3g', '--cpus=2',
               '--pids-limit=256', '--security-opt', 'label=disable',
               '--volume', f'{base}:{base}:ro', '--workdir', str(PROJECT), '--entrypoint', 'python3',
               args.container_image, '-c', BROWSER,
               str(PROJECT / 'apps/browser/chromium/tests'), str(args.browser_host)]
    browser = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, bufsize=1)
    results = []
    try:
        if json.loads(read_line(browser, 20)) != {'event': 'ready'}:
            raise RuntimeError('actual browser did not become ready')
        for index, (target, mode) in enumerate([('notes', 'keep'), ('first', 'focus'),
                                                ('notes', 'cancel'), ('first', 'keep')]):
            browser_request(browser, {'action': 'begin', 'target': target})
            row = worker_run(browser, args, index, mode)
            row['target'] = target
            results.append(row)
            args.output.write_text(json.dumps(dict(complete=False, cases=results, browser=receipt,
                container_image=args.container_image,
                live_microphone=False, compositor_journey=False, installed=False), indent=2) + '\n')
            result = args.review_dir / 'fr_fr-0-complete-1-result.json'
            if result.exists():
                destination = args.review_dir / f'concurrent-{index}-result.json'
                if destination.exists():
                    raise RuntimeError('fresh per-session recognition receipt required')
                result.rename(destination)
            print('CONCURRENT_BROWSER_CASE ' + json.dumps({key: row[key] for key in
                  ['mode', 'target', 'cancelled', 'scope_exit', 'decoder_exit', 'seconds', 'final']}), flush=True)
        args.output.write_text(json.dumps(dict(complete=True, cases=results, browser=receipt,
                container_image=args.container_image,
            live_microphone=False, compositor_journey=False, installed=False), indent=2) + '\n')
    finally:
        browser.stdin.close()
        browser.wait(timeout=45)
        stderr = browser.stderr.read(65536)
        browser.stdout.close()
        browser.stderr.close()
        if browser.returncode:
            raise RuntimeError('owned browser failed: ' + stderr)


if __name__ == '__main__':
    main()
