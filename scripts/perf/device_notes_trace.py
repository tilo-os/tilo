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
records = []
for line in Path(sys.argv[1]).read_text().splitlines():
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
# The controller collects and validates each key before sending the next one.
# Keep overlapping input metadata to detect conflicting identities, but transfer
# the current key's complete witness family only: completed families are already
# retained by collect_trace on the controller. Thirty-two witnesses per key must
# not multiply the RPC payload by thirty-two historical keys.
latest = {(row.get("process_id"), row.get("observer_session"), row.get("id"))
          for row in inputs[-1:]}
frames = [row for tag, row, _ in records if tag == markers[1]
          and (row.get("process_id"), row.get("observer_session")) in workers
          and any((row.get("process_id"), row.get("observer_session"), identity) in latest
                  for identity in row.get("input_ids", []))]
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
result = "\n".join(selected)
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
