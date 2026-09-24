#!/usr/bin/env python3
"""Real authenticated HTTPS address-form probe; never reads or writes Keychain logins.

Uses an isolated profile and inert DOM in the existing HTTPS document. No TLS
exceptions, form submissions, or existing user profile access. An explicit
--synthetic-passwords mode uses only fixed test credentials in the fresh profile
and deletes its account; failed cleanup retains the profile for recovery.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import signal
import struct
import subprocess
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('host', type=Path)
    parser.add_argument('--fixture', type=Path, default=Path(__file__).parent / 'fixtures/credential_forms.js')
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--private-denial', action='store_true', help='Verify all credential operations are denied in an ephemeral private engine')
    parser.add_argument('--synthetic-passwords', action='store_true', help='Exercise save/update/fill/delete only for this fresh profile and synthetic account')
    parser.add_argument('--prepare-viewport', action='store_true', help='Wait for an 800x600 empty-page paint before HTTPS navigation')
    parser.add_argument('--navigation-timeout', type=float, default=25, help='HTTPS navigation deadline in seconds (1–120)')
    args = parser.parse_args()
    if not 1 <= args.navigation_timeout <= 120:
        parser.error('--navigation-timeout must be between 1 and 120')
    if args.private_denial and args.synthetic_passwords:
        parser.error('--private-denial and --synthetic-passwords are mutually exclusive')
    args.output.mkdir(parents=True, exist_ok=True)
    profile = tempfile.TemporaryDirectory(prefix='qos-credential-https-', delete=False)
    report = {'profile': profile.name, 'password_store_commands_sent': 0, 'passed': False}
    process = None
    cleanup_account = None
    events = queue.Queue()
    try:
        with (args.output / 'native.log').open('wb') as log:
            process = subprocess.Popen([str(args.host.resolve()), '--profile=' + profile.name, *(['--private'] if args.private_denial else [])],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                                       start_new_session=True)
            report['pid'] = process.pid
            def read():
                def exact(n):
                    data = bytearray()
                    while len(data) < n:
                        chunk = process.stdout.read(n-len(data))
                        if not chunk:
                            raise EOFError('native host exited')
                        data.extend(chunk)
                    return data
                try:
                    while True:
                        length, = struct.unpack('<I', exact(4))
                        if not 1 <= length <= 64*1024*1024:
                            raise ValueError('invalid native frame length')
                        frame = exact(length)
                        if frame[0] == 1:
                            decoded = json.loads(frame[1:])
                            if args.synthetic_passwords and any(value in json.dumps(decoded) for value in ('qos-synthetic-password-one', 'qos-synthetic-password-two')):
                                report['public_secret_exposed'] = True
                                raise AssertionError('Synthetic secret exposed in public event')
                            events.put(decoded)
                        elif frame[0] in (2, 3):
                            offset = 1 if frame[0] == 2 else 5
                            events.put({'event': 'probe_frame', 'size': struct.unpack('<II', frame[offset:offset+8])})
                except Exception as error:
                    events.put(error)
            threading.Thread(target=read, daemon=True).start()
            def send(**command):
                frame = b'\x01' + json.dumps(command).encode()
                process.stdin.write(struct.pack('<I', len(frame)) + frame)
                process.stdin.flush()
            def until(predicate, timeout=25):
                deadline = time.monotonic()+timeout
                while True:
                    event = events.get(timeout=max(.01, deadline-time.monotonic()))
                    if isinstance(event, Exception):
                        raise event
                    if predicate(event):
                        return event
                    if time.monotonic() >= deadline:
                        raise TimeoutError('native event deadline')
            report['stage'] = 'ready'
            until(lambda e: e['event'] == 'ready')
            if args.prepare_viewport:
                report['stage'] = 'empty_page_resize'
                send(op='resize', width=800, height=600, scale_factor=1)
                until(lambda e: e['event'] == 'probe_frame' and e['size'] == (800, 600))
                report['empty_page_painted'] = True
            report['stage'] = 'https_navigation'
            send(op='navigate', url='https://example.com/')
            until(lambda e: e['event'] == 'loaded' and e.get('url', '').rstrip('/') == 'https://example.com', timeout=args.navigation_timeout)
            send(op='site_status', request_id=1, origin='https://example.com')
            status = until(lambda e: e['event'] == 'site_status' and e.get('request_id') == 1)
            assert status['security'] == 'secure' and status['content_status'] == 0 and status['certificate_status'] == 0
            report['authenticated_https'] = True
            report['stage'] = 'install_inert_fixture'
            fixture = args.fixture.read_text()
            send(op='scripts', scripts=["if(window===top && location.origin==='https://example.com'){"+fixture+";document.title='qos-fixture-ready';}"])
            until(lambda e: e['event'] == 'title' and e['title'] == 'qos-fixture-ready')
            def credential(request, action, document='', **fields):
                if action in ('save', 'update', 'fill', 'delete', 'list'):
                    report['password_store_commands_sent'] += 1
                send(op='credential_action', request_id=request, action=action, origin='https://example.com', document=document, **fields)
                return until(lambda e: e['event'] == 'credential_result' and e['request_id'] == request)
            report['stage'] = 'native_document_status'
            status = credential(2, 'status')
            if args.private_denial:
                assert not status['success']
                report['stage'] = 'private_credential_denials'
                attempts = [('discover', {}), ('discover_addresses', {}), ('list', {}),
                    ('capture', {'form': '1'}), ('save', {'capture': '1'}),
                    ('update', {'capture': '1', 'account': 'a'*64}),
                    ('fill', {'form': '1', 'account': 'a'*64}),
                    ('delete', {'account': 'a'*64}),
                    ('fill_address', {'form': '1', 'profile': {'given_name': 'Forbidden'}})]
                for request, (action, fields) in enumerate(attempts, 10):
                    denied = credential(request, action, '1', **fields)
                    assert not denied['success'] and 'private' in denied['message'].lower()
                report['private_actions_refused'] = [action for action, _ in attempts]
                report['stage'] = 'shutdown'
                send(op='shutdown')
                process.wait(timeout=35)
                assert process.returncode == 0
                report['clean_shutdown'] = True
                report['passed'] = True
                return 0
            assert status['success'], status.get('message')
            document = status['document']
            report['stage'] = 'discover_address_forms'
            forms = credential(3, 'discover_addresses', document)
            assert forms['success'] and len(forms['forms']) == 1
            form = forms['forms'][0]['id']
            report['stage'] = 'explicit_address_fill'
            result = credential(4, 'fill_address', document, form=form, profile={'given_name': 'Synthetic', 'family_name': 'Person', 'street': '123 Test Street'})
            assert result['success'], result.get('message')
            check = """if(window===top && location.origin==='https://example.com'){
              const good=document.getElementById('qos-address-given').value==='Synthetic'
                && document.getElementById('qos-address-family').value==='Person'
                && document.getElementById('qos-address-street').value==='123 Test Street'
                && !document.getElementById('qos-address').dataset.submitAttempted;
              document.title=good?'qos-address-pass':'qos-address-fail';} """
            send(op='scripts', scripts=[check])
            outcome = until(lambda e: e['event'] == 'title' and e['title'].startswith('qos-address-'))
            assert outcome['title'] == 'qos-address-pass'
            report['address_filled_without_submission'] = True
            if args.synthetic_passwords:
                report['public_secret_exposed'] = False
                username = 'qos-synthetic-fixture'
                account = hashlib.sha256(username.encode()).hexdigest()
                def script(source, title):
                    send(op='scripts', scripts=["if(window===top && location.origin==='https://example.com'){" + source + ";document.title=" + json.dumps(title) + ";}"])
                    until(lambda e: e['event'] == 'title' and e['title'] == title)
                def set_login(password, title):
                    script("document.getElementById('qos-login-username').value=" + json.dumps(username) + ";document.getElementById('qos-login-password').value=" + json.dumps(password), title)
                report['stage'] = 'synthetic_keychain_initial_list'
                listed = credential(10, 'list', document)
                assert listed['success'] and listed['accounts'] == []
                set_login('qos-synthetic-password-one', 'qos-login-prepared')
                forms = credential(11, 'discover', document)
                assert forms['success'] and len(forms['forms']) == 1
                login = forms['forms'][0]['id']
                captured = credential(12, 'capture', document, form=login)
                assert captured['success'] and 'password' not in captured
                report['stage'] = 'hidden_capture_invalidation'
                cleanup_account = account  # Preserve cleanup if a regression wrongly stores it.
                send(op='visibility', visible=False)
                refused = credential(15, 'save', document, capture=captured['capture'])
                assert not refused['success']
                send(op='visibility', visible=True)
                refused = credential(16, 'save', document, capture=captured['capture'])
                assert not refused['success']
                listed = credential(17, 'list', document)
                assert listed['success'] and listed['accounts'] == []
                cleanup_account = None
                report['hidden_capture_cannot_be_revived'] = True
                captured = credential(18, 'capture', document, form=login)
                assert captured['success']
                report['stage'] = 'synthetic_keychain_save'
                cleanup_account = account  # A timed-out save may still have reached Keychain.
                saved = credential(13, 'save', document, capture=captured['capture'])
                assert saved['success']
                listed = credential(14, 'list', document)
                assert listed['success'] and len(listed['accounts']) == 1
                assert listed['accounts'][0]['id'] == account
                for request, password in [(20, 'qos-synthetic-password-one'), (30, 'qos-synthetic-password-two')]:
                    if request == 30:
                        report['stage'] = 'synthetic_keychain_update'
                        set_login(password, 'qos-login-update-prepared')
                        captured = credential(27, 'capture', document, form=login)
                        assert captured['success']
                        updated = credential(28, 'update', document, capture=captured['capture'], account=account)
                        assert updated['success']
                    script("document.getElementById('qos-login-username').value='';document.getElementById('qos-login-password').value=''", 'qos-login-cleared-' + str(request))
                    report['stage'] = 'synthetic_keychain_fill_' + str(request)
                    filled = credential(request, 'fill', document, form=login, account=account)
                    assert filled['success'] and 'password' not in filled
                    check = "const good=document.getElementById('qos-login-username').value===" + json.dumps(username) + "&&document.getElementById('qos-login-password').value===" + json.dumps(password) + "&&!document.getElementById('qos-login').dataset.submitAttempted;document.title=good?'qos-login-pass-" + str(request) + "':'qos-login-fail'"
                    send(op='scripts', scripts=["if(window===top && location.origin==='https://example.com'){" + check + ";}"])
                    verified = until(lambda e: e['event'] == 'title' and e['title'].startswith(('qos-login-pass-', 'qos-login-fail')))
                    assert verified['title'] == 'qos-login-pass-' + str(request)
                report['stage'] = 'cross_origin_refusal'
                send(op='credential_action', request_id=39, action='fill', origin='https://example.org', document=document, form=login, account=account)
                report['password_store_commands_sent'] += 1
                refused = until(lambda e: e['event'] == 'credential_result' and e['request_id'] == 39)
                assert not refused['success']
                report['cross_origin_fill_refused'] = True
                report['stage'] = 'synthetic_keychain_delete'
                deleted = credential(40, 'delete', document, account=account)
                assert deleted['success']
                cleanup_account = None
                listed = credential(41, 'list', document)
                assert listed['success'] and listed['accounts'] == []
                report['synthetic_save_update_fill_delete'] = True
            send(op='visibility', visible=False)
            hidden = credential(5, 'fill_address', document, form=form, profile={'given_name': 'Forbidden'})
            assert not hidden['success']
            report['hidden_fill_refused'] = True
            report['passed'] = True
            report['stage'] = 'shutdown'
            send(op='shutdown')
            process.wait(timeout=35)
            assert process.returncode == 0
            report['clean_shutdown'] = True
    except Exception as error:
        report['error_type'] = type(error).__name__
        report['passed'] = False
        if process is not None and process.poll() is None:
            try:
                sampled = subprocess.run(['sample', str(process.pid), '2', '1', '-file', str(args.output / 'timeout-sample.txt')],
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=8)
                report['timeout_sample_saved'] = sampled.returncode == 0
            except (OSError, subprocess.TimeoutExpired):
                report['timeout_sample_saved'] = False
    finally:
        if cleanup_account is not None and process is not None and process.poll() is None:
            try:
                cleaned = credential(1000, 'delete', document, account=cleanup_account)
                if cleaned['success']:
                    cleanup_account = None
            except Exception:
                pass
        report['synthetic_account_cleanup_pending'] = cleanup_account is not None
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
            report['forced_cleanup'] = True
        if process is not None:
            process.stdin.close()
            process.stdout.close()
        if cleanup_account is None:
            profile.cleanup()
        report['profile_removed'] = not Path(profile.name).exists()
        (args.output / 'result.json').write_text(json.dumps(report, indent=2)+'\n')
        print(json.dumps(report))
    return 0 if report['passed'] else 1

if __name__ == '__main__':
    raise SystemExit(main())
