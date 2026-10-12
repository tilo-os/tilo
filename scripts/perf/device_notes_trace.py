"""Read bounded, content-free Notes evidence before the flight log rolls over."""

# Executed by the existing tilo-vm slot exec path. Select the latest bounded
# input cohort and only its exact worker frames, delivery serials and feedback.
# The controller preserves each cohort locally before the next physical key.
READ_TRACE_SCRIPT = r"""
import json
import sys
from pathlib import Path
markers = ("tilo-physical-input ", "tilo-physical-frame ",
           "tilo-physical-delivery ", "tilo-display-frame ",
           "tilo-native-send ", "tilo-native-main-stages ",
           "tilo-native-worker ", "tilo-native-worker-handoff ",
           "tilo-native-present-call ", "tilo-native-gpu-stages ")
phase_fields = set("serial event revision phase_timing queue_wait_us render_us total_us "
                   "inflight_at_enqueue input_age_at_enqueue_us encode_submit_us "
                   "gpu_completion_wait_us bump_settle_us backdrop_us kms_retirement_us "
                   "cpu_copy_us readback_bytes output_bytes physical_presented_time_ns "
                   "physical_sequence".split())
optional_numbers = {"input_age_at_enqueue_us", "physical_presented_time_ns", "physical_sequence"}
lifecycle = []
records = []
for line in Path(sys.argv[1]).read_text().splitlines():
    if "FRAME-LIFECYCLE serial=" in line:
        suffix = line.partition("FRAME-LIFECYCLE ")[2]
        fields = {}
        for token in suffix.split():
            key, separator, value = token.partition("=")
            if not separator or key not in phase_fields:
                continue  # Never export arbitrary diagnostic/user text.
            if key in fields:
                raise SystemExit("Duplicate compositor lifecycle field")
            valid = ((key == "event" and value == "render-retired")
                     or (key == "phase_timing" and value in ("true", "false"))
                     or (key in optional_numbers and value == "none")
                     or (key not in ("event", "phase_timing") and value.isascii() and value.isdecimal()))
            if not valid:
                raise SystemExit("Invalid compositor lifecycle field")
            fields[key] = value
        proof = (fields.get("physical_presented_time_ns"), fields.get("physical_sequence"))
        lifecycle.append((proof, "FRAME-LIFECYCLE " + " ".join(key + "=" + value for key, value in fields.items())))
        continue
    for marker in markers:
        if marker in line:
            value = line[line.index(marker) + len(marker):]
            try:
                row = json.loads(value)
            except json.JSONDecodeError:
                # A bounded flight log may begin in the middle of a record.
                continue
            if isinstance(row, dict):
                records.append((marker, row, marker + value))
            break
inputs = [row for tag, row, _ in records
          if tag == markers[0] and (row.get("before") or {}).get("selected_page") == sys.argv[2]][-32:]
workers = {(row.get("process_id"), row.get("observer_session")) for row in inputs}
identities = {row.get("id") for row in inputs}
serials = {row.get("wayland_serial") for row in inputs}
frames = [row for tag, row, _ in records if tag == markers[1]
          and (row.get("process_id"), row.get("observer_session")) in workers
          and any(identity in identities for identity in row.get("input_ids", []))]
frame_ids = {(row.get("process_id"), row.get("observer_session"), row.get("cpu_frame_id")) for row in frames}
selected = []
for tag, row, original in records:
    worker = (row.get("process_id"), row.get("observer_session"))
    if ((tag == markers[0] and worker in workers and row.get("id") in identities)
            or (tag == markers[1] and worker + (row.get("cpu_frame_id"),) in frame_ids)
            or (tag == markers[2] and row.get("wayland_serial") in serials)
            or (tag == markers[3] and row.get("backend") == "wayland"
                and worker + (row.get("cpu_frame_id"),) in frame_ids)
            or (tag in markers[4:]
                and worker + (row.get("cpu_frame_id"),) in frame_ids)):
        selected.append(original)
proofs = {(str(row.get("presented_time_ns")), str(row.get("sequence")))
          for tag, row, _ in records if tag == markers[3]
          and (row.get("process_id"), row.get("observer_session"), row.get("cpu_frame_id")) in frame_ids
          and row.get("clock_id") == 1 and row.get("discarded") is False
          and type(row.get("flags")) is int and row["flags"] & 7 == 7
          and type(row.get("presented_time_ns")) is int and type(row.get("sequence")) is int}
# Preserve exact selected outcomes even outside the bounded background window.
selected.extend(original for proof, original in lifecycle if proof in proofs)
selected.extend(original for _, original in lifecycle[-128:])
result = "\n".join(dict.fromkeys(selected))
if len(result.encode()) > 524288:
    raise SystemExit("Notes trace cohort exceeds the bounded collection limit")
print(result)
"""


def collect_trace(device, log_path, page, records):
    """Preserve overlap exactly; distinct duplicate identities still fail analysis."""
    raw = device.execute(["/usr/bin/python3", "-c", READ_TRACE_SCRIPT, log_path, page])
    for line in raw.splitlines():
        if line:
            records.setdefault(line, None)
    return list(records)
