"""Verify retained observations of real dictation field preparation.

Usage: python3 tools/release/verify_dictation_field_setup.py RECEIPT.json
Every observation must name the receipt's control binary and owned socket.
This checks the exact edit, original active owner, trusted prompt and prepared
cancellation. It grants no runtime/model authority and never qualifies speech.
"""
import json
import pathlib
import sys

TRUSTED_PROMPT = "tilo.shell.trusted-frame/dictation-microphone"


def require(condition, message):
    # These gates must also execute when Python is run with -O.
    if not condition:
        raise ValueError(message)


def command(record, result, args):
    prefix = [record["binaries"]["tilo-control"]["path"], "--socket", record["owned_socket"]]
    require(result["argv"] == prefix + args, "foreign or stale owned socket observation")
    require(result["exit"] == 0, "control observation failed")
    return result["stdout"]


def verify(record):
    require(type(record["kernel_peer_pid"]) is int and record["kernel_peer_pid"] > 0 and record["kernel_peer_pid"] == record["owned_pid"], "owned socket peer missing or mismatched")
    require(type(record["kernel_peer_uid"]) is int and record["kernel_peer_uid"] >= 0 and record["kernel_peer_uid"] == record["runtime_uid"], "owned UID missing or mismatched")
    require(
        all(record["cleanup"].get(k) is True for k in ["cleanup_complete", "pid_reaped", "socket_removed"]),
        "owned cleanup unobserved",
    )
    require(not record.get("field_setup_error"), record.get("field_setup_error"))
    journal = record.get("field_setup_journal", [])
    stages = {entry["stage"]: entry for entry in journal}
    require(len(stages) == len(journal), "ambiguous duplicate field stage")
    required = [
        "public-baseline-keyboard-insertion",
        "original-field-consent-prompt-observed",
        "prepared-consent-canceled-without-capture",
    ]
    require(
        all(name in stages for name in required),
        "success receipts without field/prompt/edit observations cannot be credited",
    )
    observed = []
    for name in required:
        stage = stages[name]
        state = json.loads(command(record, stage["state"], ["state-dump"]))["chrome"]
        targets = command(record, stage["targets"], ["list-targets"])
        observed.append((state, targets))
    (before, _), (prepared, prepared_targets), (canceled, canceled_targets) = observed
    expected = record["public_text_expected"]
    command(record, stages[required[0]]["result"], ["type", expected])
    cancel_result = command(
        record,
        stages[required[2]]["result"],
        ["actions", "invoke", "actions", "dictation.cancel"],
    )
    require(json.loads(cancel_result)["receipt"]["update"]["state"] == "succeeded", "cancel request failed")
    require(before["palette"]["query"] == expected, "successful type did not insert exact public text")
    original = before["seat_focus"]["text_input"]
    require(original is not None, "no focused text-input surface")
    require(prepared["seat_focus"]["text_input"] == original, "preparation redirected the field")
    require(prepared["seat_focus"]["text_input_active"] is True, "prepared target is not active")
    require(TRUSTED_PROMPT in prepared_targets, "trusted consent prompt not observed")
    require(prepared["palette"]["query"] == expected, "preparation edited the original field")
    require(canceled["palette"]["query"] == expected, "cancel edited the original field")
    require(TRUSTED_PROMPT not in canceled_targets, "cancel left consent alive")
    require(canceled["seat_focus"]["text_input"] == original, "cancel redirected the field")
    require(record["mic_attempts"] == 0 and record["capture_started"] is False, "this qualification cannot authorize capture")
    return {
        "field_setup_verified": True,
        "keyboard_insertion_verified": True,
        "prepared_owner_and_consent_verified": True,
        "prepared_cancel_verified": True,
        "speech_acceptance_verified": False,
        "running_executable_readback_verified": record["process_executable_readback_equal"],
    }


if __name__ == "__main__":
    try:
        result = verify(json.loads(pathlib.Path(sys.argv[1]).read_text()))
    except (IndexError, KeyError, TypeError, ValueError, OSError) as error:
        print(json.dumps({"field_setup_verified": False, "speech_acceptance_verified": False, "failure": str(error)}))
        sys.exit(1)
    print(json.dumps(result))
