"""Verify retained observations of real dictation field preparation.

Usage: python3 tools/release/verify_dictation_field_setup.py RECEIPT.json
This checks the field, exact edit, original active owner, trusted prompt and
prepared cancellation. Successful action receipts alone fail. It grants no
runtime/model authority and never qualifies speech recognition or capture.
"""
import json, pathlib, sys

def verify(record):
    assert record['kernel_peer_pid']==record['owned_pid'], 'owned socket peer mismatch'
    assert record['kernel_peer_uid']==record['runtime_uid'], 'owned UID mismatch'
    cleanup=record['cleanup']
    assert all(cleanup.get(k) is True for k in ['cleanup_complete','pid_reaped','socket_removed']), 'owned cleanup unobserved'
    assert not record.get('field_setup_error'), record.get('field_setup_error')
    stages={x['stage']:x for x in record.get('field_setup_journal',[])}
    required=['public-baseline-keyboard-insertion','original-field-consent-prompt-observed','prepared-consent-canceled-without-capture']
    assert all(k in stages for k in required), 'success receipts without field/prompt/edit observations cannot be credited'
    expected=record['public_text_expected']
    def chrome(stage):
        result=stages[stage]['state']
        assert result['exit']==0, 'state read failed'
        return json.loads(result['stdout'])['chrome']
    before=chrome(required[0]); prepared=chrome(required[1]); canceled=chrome(required[2])
    assert before['palette']['query']==expected, 'successful type did not insert exact public text'
    original=before['seat_focus']['text_input']
    assert original is not None, 'no focused text-input surface'
    assert prepared['seat_focus']['text_input']==original, 'preparation redirected the field'
    assert prepared['seat_focus']['text_input_active'] is True, 'prepared target is not active'
    assert 'tilo.shell.trusted-frame/dictation-microphone' in stages[required[1]]['targets']['stdout'], 'trusted consent prompt not observed'
    assert prepared['palette']['query']==expected, 'preparation edited the original field'
    assert canceled['palette']['query']==expected, 'cancel edited the original field'
    assert 'tilo.shell.trusted-frame/dictation-microphone' not in stages[required[2]]['targets']['stdout'], 'cancel left consent alive'
    assert canceled['seat_focus']['text_input']==original, 'cancel redirected the field'
    assert record['mic_attempts']==0 and record['capture_started'] is False, 'this qualification cannot authorize capture'
    return {'field_setup_verified':True,'keyboard_insertion_verified':True,'prepared_owner_and_consent_verified':True,'prepared_cancel_verified':True,'speech_acceptance_verified':False,'running_executable_readback_verified':record['process_executable_readback_equal']}

if __name__=='__main__':
    try:
        result=verify(json.loads(pathlib.Path(sys.argv[1]).read_text()))
    except (AssertionError,KeyError,ValueError) as error:
        print(json.dumps({'field_setup_verified':False,'speech_acceptance_verified':False,'failure':str(error)}))
        sys.exit(1)
    print(json.dumps(result))
