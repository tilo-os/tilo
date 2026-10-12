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
diagnostics = []
diagnostic_fields = {
 "tilo-foreign-capture ": set("event clock_id surface_id capture_token capture_seq prepare_ns enqueue_ns worker_start_ns fence_start_ns fence_end_ns native_import_end_ns copy_submit_ns copy_submit_return_ns copy_end_ns staged_ns barrier_ns barrier_return_ns activated_ns accepted shared_gpu borrowed_gpu fence_waited capture_limit".split()),
 "tilo-compositor-feedback ": set("event clock_id event_time_ns composed_time_ns pipe_serial surface_id capture_seq physical_presented_time_ns physical_sequence omitted_pixels".split()),
 "tilo-present-pipe ": set("event clock_id pipe_serial enqueue_ns render_start_ns render_end_ns".split()),
}
diagnostic_events = ({"imported", "activated", "budget-exhausted"}, {"submitted", "completed"}, {"retired"})

for line in Path(sys.argv[1]).read_text().splitlines():
    for index, (marker, fields) in enumerate(diagnostic_fields.items()):
        if marker not in line:
            continue
        value = line[line.index(marker) + len(marker):]
        if len(value) > 4096 or len(diagnostics) >= 10000:
            raise SystemExit("Capture diagnostic records exceed their bound")
        try:
            original = json.loads(value)
        except json.JSONDecodeError:
            raise SystemExit("Invalid capture diagnostic JSON") from None
        if not isinstance(original, dict):
            raise SystemExit("Invalid capture diagnostic record")
        row = {key: value for key, value in original.items() if key in fields}
        if row.get("event") not in diagnostic_events[index]:
            raise SystemExit("Invalid capture diagnostic event")
        for key, value in row.items():
            valid = (key == "event" or (key in ("accepted", "shared_gpu", "borrowed_gpu", "fence_waited") and type(value) is bool)
                     or (key not in ("event", "accepted", "shared_gpu", "borrowed_gpu", "fence_waited")
                         and ((value is None and (key.endswith("_ns") or key == "physical_sequence"))
                              or (type(value) is int and 0 <= value <= 2**64 - 1))))
            if not valid:
                raise SystemExit("Invalid capture diagnostic field")
        diagnostics.append((marker, row, marker + json.dumps(row, sort_keys=True)))
        break
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
# Keep the exact selected physical outcome -> composed pixels -> capture token
# witness family. Never transfer arbitrary log fields or choose nearby records.
completed = [row for tag, row, _ in diagnostics if tag == "tilo-compositor-feedback "
             and row.get("event") == "completed"
             and (str(row.get("physical_presented_time_ns")), str(row.get("physical_sequence"))) in proofs]
pixels = {(row.get("pipe_serial"), row.get("surface_id"), row.get("capture_seq")) for row in completed}
serials = {row[0] for row in pixels}
activations = [row for tag, row, _ in diagnostics if tag == "tilo-foreign-capture "
               and row.get("event") == "activated"
               and any((row.get("surface_id"), row.get("capture_seq")) == pixel[1:] for pixel in pixels)]
tokens = {(row.get("surface_id"), row.get("capture_token")) for row in activations}
selected_diagnostics = set()
for tag, row, original in diagnostics:
    selected_row = ((tag == "tilo-compositor-feedback " and (row.get("pipe_serial"), row.get("surface_id"), row.get("capture_seq")) in pixels)
                    or (tag == "tilo-present-pipe " and row.get("pipe_serial") in serials)
                    or (tag == "tilo-foreign-capture " and ((row.get("surface_id"), row.get("capture_token")) in tokens or row.get("event") == "budget-exhausted")))
    if selected_row:
        identity = (tag, row.get("event"), row.get("pipe_serial"), row.get("surface_id"), row.get("capture_seq"), row.get("capture_token"))
        if identity in selected_diagnostics:
            raise SystemExit("Duplicate selected capture diagnostic identity")
        selected_diagnostics.add(identity)
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
