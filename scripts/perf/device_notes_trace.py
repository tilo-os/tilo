"""Read bounded, content-free Notes evidence before the flight log rolls over."""

# Executed by the existing tilo-vm slot exec path. Select the latest bounded
# input cohort and only its exact worker frames, delivery serials and feedback.
# The controller preserves each cohort locally before the next physical key.
READ_TRACE_SCRIPT = r"""
import json
import sys
from pathlib import Path
import math
import re
import uuid
palette_mode = len(sys.argv) > 6 and sys.argv[4] == "palette"
palette_rows = []

def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise SystemExit("Duplicate palette trace field")
        result[key] = value
    return result

def bounded(value):
    return type(value) is int and 0 <= value < 2**64

def random_identity(value):
    try:
        parsed = uuid.UUID(value)
    except (ValueError, TypeError, AttributeError):
        raise SystemExit("Invalid palette worker identity") from None
    if str(parsed) != value or parsed.version != 4:
        raise SystemExit("Invalid palette worker identity")
    return value

def public_id(value):
    if not isinstance(value, str) or len(value) > 4096:
        raise SystemExit("Invalid public action identity")
    if re.fullmatch(r"[A-Za-z0-9_.:@/+-]{1,512}", value):
        return value
    parts = value.split("\x1f")
    semantic = ((len(parts) == 5 and parts[0] == "directory")
                or (len(parts) == 3 and parts[0] == "directory-resource")
                or (len(parts) == 2 and parts[0] in ("job", "job-cancel"))
                or (len(parts) == 6 and re.fullmatch(r"so\.tilo\.[a-z0-9_.-]+", parts[0])
                    and parts[1] in ("none", "read", "comment", "write", "invite", "admin")))
    # These are minted resource/action addresses, not query-bearing live:* ids.
    if not semantic or any(not part or len(part) > 512
                           or any(not 32 <= ord(char) < 127 for char in part) for part in parts):
        raise SystemExit("Invalid public action identity")
    return value

def scrub(value, fields):
    if not isinstance(value, dict):
        raise SystemExit("Invalid palette trace object")
    result = {}
    for key, item in value.items():
        if key not in fields:
            continue
        if item is not None and not (type(item) is bool or bounded(item)
            or (type(item) is float and math.isfinite(item))):
            if not isinstance(item, str) or len(item) > 4096:
                raise SystemExit("Invalid palette scalar")
            if key in ("query_hash", "published_revision_hash"):
                if not re.fullmatch(r"[0-9a-f]{64}", item):
                    raise SystemExit("Invalid content-free digest")
            elif key in ("observer_session", "palette_observer_session", "invocation"):
                random_identity(item)
            elif key == "kind":
                if item not in ("physical_key", "physical_pointer_motion", "physical_pointer_button"):
                    raise SystemExit("Invalid palette input kind")
            elif key == "source":
                if item not in ("semantic", "native_unstamped", "controlled_input", "physical_key", "physical_pointer_motion", "physical_pointer_button"):
                    raise SystemExit("Invalid action source")
            elif key in ("app", "application"):
                if item != "so.tilo.settings":
                    raise SystemExit("Invalid Settings application")
            elif key == "clock":
                if item != "clock_monotonic_seconds":
                    raise SystemExit("Invalid broker clock")
            elif key == "backend":
                if item != "wayland":
                    raise SystemExit("Invalid presentation backend")
            else:
                public_id(item)
        result[key] = item
    return result

def palette_snapshot(value):
    fields = "visible mounted query_hash query_bytes results_generation published_revision_hash query_current selected_entry_id hovered_entry_id".split()
    result = scrub(value, fields)
    for key in ("bounds", "query_bounds"):
        item = value.get(key)
        if item is not None and (not isinstance(item, list) or len(item) != 4
            or any(type(v) not in (int, float) or not math.isfinite(v) for v in item)):
            raise SystemExit("Invalid palette geometry")
        if key in value:
            result[key] = item
    rows = value.get("rows")
    if not isinstance(rows, list) or len(rows) > 256:
        raise SystemExit("Invalid bounded palette rows")
    result["rows"] = []
    for row in rows:
        clean = scrub(row, "entry_id result_index visible_index provider_id action_id selected hovered".split())
        bounds = row.get("bounds")
        if not isinstance(bounds, list) or len(bounds) != 4 or any(type(v) not in (int, float) or not math.isfinite(v) for v in bounds):
            raise SystemExit("Invalid palette row geometry")
        clean["bounds"] = bounds
        result["rows"].append(clean)
    return result

def settings_snapshot(value):
    result = scrub(value, ("app",))
    for key in ("navigation", "rendered_navigation"):
        if key in value:
            result[key] = (None if value[key] is None else scrub(value[key],
                "selected_pane requested_pane path ready pending content_visible".split()))
    return result

palette_fields = {
 "tilo-palette-input ": "schema clock_id process_id observer_session id kind input_time_ns dispatch_start_ns dispatch_end_ns",
 "tilo-palette-frame ": "schema clock_id process_id observer_session pipe_serial scene_revision draw_time_ns submit_time_ns",
 "tilo-action-submission ": "invocation application source clock_id activation_dispatch_ns input_time_ns submission_time_ns process_id palette_input_id palette_observer_session palette_target_provider palette_target_action",
 "tilo-broker-input ": "process_id observer_session invocation clock handler_returned_ok handler_dispatch_lo_s handler_dispatch_hi_s handler_return_lo_s handler_return_hi_s",
 "tilo-broker-frame ": "process_id observer_session cpu_frame_id",
}
markers = ("tilo-physical-input ", "tilo-physical-frame ",
           "tilo-physical-delivery ", "tilo-display-frame ",
           "tilo-native-send ", "tilo-native-main-stages ",
           "tilo-native-worker ", "tilo-native-worker-handoff ",
           "tilo-native-present-call ", "tilo-native-gpu-stages ")
pointer_marker = "tilo-physical-pointer-delivery "
phase_fields = set("serial event revision phase_timing queue_wait_us render_us total_us "
                   "inflight_at_enqueue input_age_at_enqueue_us encode_submit_us "
                   "gpu_completion_wait_us bump_settle_us backdrop_us kms_retirement_us "
                   "cpu_copy_us readback_bytes output_bytes physical_presented_time_ns "
                   "physical_sequence render_work target_width target_height render_attempts "
                   "sparse_attempts full_attempts damage_admitted".split())
optional_numbers = {"input_age_at_enqueue_us", "physical_presented_time_ns", "physical_sequence"}
submissions = []
gpu_submissions = []
gpu_fields = set("serial revision scope status process_id renderer_generation encoder_supported encoder_enabled external_queue_work first_recording last_recording recording_count measured_count stale_count recording_ns post_render_ns".split())
submission_fields = set("serial status scene_count command_count finish_count submit_count scene_record_us command_encode_us encoder_finish_us queue_submit_us sparse_marker_submit_us".split())
lifecycle = []
records = []
diagnostics = []
diagnostic_fields = {
 "tilo-foreign-capture ": set("event clock_id surface_id capture_token capture_seq prepare_ns enqueue_ns worker_start_ns fence_start_ns fence_end_ns fence_ready fence_poll_calls fence_poll_events native_import_end_ns copy_submit_ns copy_encoder_finish_return_ns copy_submit_return_ns copy_end_ns staged_ns barrier_ns barrier_return_ns activated_ns accepted shared_gpu capture_limit".split()),
 "tilo-compositor-feedback ": set("event clock_id event_time_ns composed_time_ns pipe_serial surface_id capture_seq physical_presented_time_ns physical_sequence omitted_pixels".split()),
 "tilo-present-pipe ": set("event clock_id pipe_serial enqueue_ns render_start_ns render_end_ns".split()),
}
diagnostic_events = ({"imported", "activated", "budget-exhausted"}, {"submitted", "completed"}, {"retired"})
if palette_mode:
    if not sys.argv[6].isascii() or not sys.argv[6].isdecimal() or int(sys.argv[6]) == 0:
        raise SystemExit("Explicit palette process identity required")
    palette_owner = int(sys.argv[6]), random_identity(sys.argv[3])
    settings_owner = int(sys.argv[7]), random_identity(sys.argv[8])
    if not bounded(settings_owner[0]) or settings_owner[0] == 0 or not bounded(int(sys.argv[5])):
        raise SystemExit("Explicit Settings worker and palette boundary required")
if palette_mode:
    with Path(sys.argv[1]).open("rb") as source:
        payload = source.read(32 * 1024 * 1024 + 1)
    if len(payload) > 32 * 1024 * 1024:
        raise SystemExit("Palette source exceeds its byte bound")
    text = payload.decode("utf-8")
else:
    text = Path(sys.argv[1]).read_text()
for line in text.splitlines():
    if palette_mode:
        found = False
        for tag, fields in palette_fields.items():
            if tag not in line:
                continue
            found = True
            payload = line.partition(tag)[2]
            if len(payload.encode()) > 65536 or len(palette_rows) >= 10000:
                raise SystemExit("Palette capture exceeds its bound")
            try:
                original = json.loads(payload, object_pairs_hook=pairs)
            except json.JSONDecodeError:
                raise SystemExit("Invalid palette capture JSON") from None
            row = scrub(original, fields.split())
            for key in ("before", "after", "state"):
                if key in original:
                    row[key] = (palette_snapshot(original[key]) if tag.startswith("tilo-palette-")
                                else settings_snapshot(original[key]))
            for key in ("input_ids", "invocations"):
                if key in original:
                    items = original[key]
                    if not isinstance(items, list) or len(items) > 32:
                        raise SystemExit("Invalid palette cause list")
                    if key == "input_ids" and any(not bounded(item) for item in items):
                        raise SystemExit("Invalid palette input identity")
                    if key == "invocations":
                        for item in items: random_identity(item)
                    row[key] = items
            palette_rows.append((tag, row, tag + json.dumps(row, sort_keys=True)))
            break
        if found:
            continue
    if "FRAME-GPU-SUBMISSION " in line:
        suffix = line.partition("FRAME-GPU-SUBMISSION ")[2]
        if len(suffix) > 4096 or len(gpu_submissions) >= 10000:
            raise SystemExit("GPU submission records exceed their bound")
        fields = {}
        for token in suffix.split():
            key, separator, value = token.partition("=")
            if not separator or key not in gpu_fields:
                continue
            if key in fields:
                raise SystemExit("Duplicate GPU submission field")
            valid = ((key == "scope" and value == "encoder_before_query_resolve")
                     or (key == "status" and value in ("complete", "incomplete", "unsupported", "disabled", "missing"))
                     or (key in ("encoder_supported", "encoder_enabled", "external_queue_work") and value in ("true", "false"))
                     or (key in ("renderer_generation", "first_recording", "last_recording", "recording_ns", "post_render_ns") and value == "none")
                     or (key not in ("scope", "status", "encoder_supported", "encoder_enabled", "external_queue_work") and value.isascii() and value.isdecimal()))
            if not valid:
                raise SystemExit("Invalid GPU submission field")
            fields[key] = value
        gpu_submissions.append((fields.get("serial"), "FRAME-GPU-SUBMISSION " + " ".join(key + "=" + value for key, value in fields.items())))
        continue
    if "FRAME-SUBMISSION serial=" in line:
        suffix = line.partition("FRAME-SUBMISSION ")[2]
        if len(suffix) > 4096 or len(submissions) >= 10000:
            raise SystemExit("Submission records exceed their bound")
        fields = {}
        for token in suffix.split():
            key, separator, value = token.partition("=")
            if not separator or key not in submission_fields:
                continue
            if key in fields:
                raise SystemExit("Duplicate submission field")
            valid = ((key == "status" and value in ("complete", "incomplete", "invalid", "missing"))
                     or (key.endswith("_us") and value == "none")
                     or (key != "status" and value.isascii() and value.isdecimal()))
            if not valid:
                raise SystemExit("Invalid submission field")
            fields[key] = value
        submissions.append((fields.get("serial"), "FRAME-SUBMISSION " + " ".join(key + "=" + value for key, value in fields.items())))
        continue
    if "FRAME-LIFECYCLE serial=" in line:
        suffix = line.partition("FRAME-LIFECYCLE ")[2]
        if palette_mode and (len(suffix) > 4096 or len(lifecycle) >= 10000):
            raise SystemExit("Palette lifecycle capture exceeds its bound")
        fields = {}
        for token in suffix.split():
            key, separator, value = token.partition("=")
            if not separator or key not in phase_fields:
                continue  # Never export arbitrary diagnostic/user text.
            if key in fields:
                raise SystemExit("Duplicate compositor lifecycle field")
            valid = ((key == "event" and value == "render-retired")
                     or (key in ("phase_timing", "render_work", "damage_admitted") and value in ("true", "false"))
                     or (key in optional_numbers and value == "none")
                     or (key not in ("event", "phase_timing", "render_work", "damage_admitted") and value.isascii() and value.isdecimal()))
            if not valid:
                raise SystemExit("Invalid compositor lifecycle field")
            fields[key] = value
        proof = (fields.get("physical_presented_time_ns"), fields.get("physical_sequence"))
        lifecycle.append((proof, "FRAME-LIFECYCLE " + " ".join(key + "=" + value for key, value in fields.items())))
        continue
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
            valid = (key == "event" or (key == "fence_ready" and value is None) or (key in ("accepted", "shared_gpu", "fence_ready") and type(value) is bool)
                     or (key not in ("event", "accepted", "shared_gpu", "fence_ready")
                         and ((value is None and (key.endswith("_ns") or key in ("physical_sequence", "fence_poll_calls", "fence_poll_events")))
                              or (type(value) is int and 0 <= value <= 2**64 - 1))))
            if not valid:
                raise SystemExit("Invalid capture diagnostic field")
        diagnostics.append((marker, row, marker + json.dumps(row, sort_keys=True)))
        break
    for marker in (*markers, pointer_marker):
        if marker in line:
            value = line[line.index(marker) + len(marker):]
            try:
                row = json.loads(value)
            except json.JSONDecodeError:
                # A bounded flight log may begin in the middle of a record.
                continue
            if isinstance(row, dict):
                if palette_mode:
                    if marker != "tilo-display-frame ":
                        break
                    row = scrub(row, "process_id observer_session backend cpu_frame_id clock_id flags discarded presented_time_ns sequence worker_id worker_frame_ix".split())
                    value = json.dumps(row, sort_keys=True)
                if marker == pointer_marker:
                    allowed = {"pointer_id", "enter_serial", "motion_ordinal", "time_ms",
                               "surface_id", "clock_id", "delivery_time_ns", "input_time_ns"}
                    row = {key: field for key, field in row.items() if key in allowed}
                    for key, field in row.items():
                        if not ((field is None and key in ("time_ms", "input_time_ns"))
                                or (type(field) is int and 0 <= field < 2**64)):
                            raise SystemExit("Invalid pointer delivery field")
                    value = json.dumps(row, sort_keys=True)
                records.append((marker, row, marker + value))
            break
application = sys.argv[4] if len(sys.argv) > 4 else "so.tilo.notes"
observer = sys.argv[3] if len(sys.argv) > 3 else None
minimum = int(sys.argv[5]) if len(sys.argv) > 5 else 0
if application == "so.tilo.settings":
    import uuid
    if minimum < 0 or observer is None:
        raise SystemExit("Settings hover requires explicit worker and input boundary")
    try:
        identity = uuid.UUID(observer)
    except ValueError:
        raise SystemExit("Invalid Settings worker identity") from None
    if identity.version != 4 or str(identity) != observer or sys.argv[2] != "settings.nav":
        raise SystemExit("Invalid Settings hover scope")
    inputs = [row for tag, row, _ in records if tag == markers[0]
              and row.get("observer_session") == observer
              and (row.get("before") or {}).get("app") == application
              and (type(row.get("id")) is not int or row["id"] > minimum)][-32:]
elif application == "so.tilo.notes":
    inputs = [row for tag, row, _ in records
              if tag == markers[0] and (row.get("before") or {}).get("selected_page") == sys.argv[2]][-32:]
elif application == "palette" and palette_mode:
    inputs = []
else:
    raise SystemExit("Unsupported physical trace application")
workers = {(row.get("process_id"), row.get("observer_session")) for row in inputs}
identities = {row.get("id") for row in inputs}
serials = {row.get("wayland_serial") for row in inputs}
pointer_keys = {(row.get("pointer_id"), row.get("pointer_enter_serial"),
                 row.get("pointer_motion_ordinal"), row.get("pointer_time_ms")) for row in inputs}
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
if palette_mode:
    owner = lambda row: (row.get("process_id"), row.get("observer_session"))
    palette_inputs = [row for tag, row, _ in palette_rows if tag == "tilo-palette-input "
                      and owner(row) == palette_owner and type(row.get("id")) is int
                      and row["id"] > minimum][-32:]
    # Preserve this event before the next input; previous completed families
    # already live in the controller's records. Avoid multiplying RPC output
    # by 32 historical rendered snapshots while retaining current duplicates.
    palette_ids = {row["id"] for row in palette_inputs[-1:]}
    palette_frames = [row for tag, row, _ in palette_rows if tag == "tilo-palette-frame "
                      and owner(row) == palette_owner and palette_ids.intersection(row.get("input_ids", []))]
    palette_serials = {str(row.get("pipe_serial")) for row in palette_frames}
    actions = [row for tag, row, _ in palette_rows if tag == "tilo-action-submission "
               and row.get("process_id") == palette_owner[0]
               and row.get("palette_observer_session") == palette_owner[1]
               and row.get("palette_input_id") in palette_ids]
    invocations = {row.get("invocation") for row in actions}
    broker_frames = [row for tag, row, _ in palette_rows if tag == "tilo-broker-frame "
                     and owner(row) == settings_owner and invocations.intersection(row.get("invocations", []))]
    frame_ids.update(settings_owner + (row.get("cpu_frame_id"),) for row in broker_frames)
    for tag, row, original in palette_rows:
        if ((tag == "tilo-palette-input " and owner(row) == palette_owner and row.get("id") in palette_ids)
            or (tag == "tilo-palette-frame " and row in palette_frames)
            or (tag == "tilo-action-submission " and row in actions)
            or (tag == "tilo-broker-input " and owner(row) == settings_owner and row.get("invocation") in invocations)
            or (tag == "tilo-broker-frame " and row in broker_frames)):
            selected.append(original)
for tag, row, original in records:
    worker = (row.get("process_id"), row.get("observer_session"))
    if ((tag == markers[0] and worker in workers and row.get("id") in identities)
            or (tag == markers[1] and worker + (row.get("cpu_frame_id"),) in frame_ids)
            or (tag == markers[2] and row.get("wayland_serial") in serials)
            or (tag == pointer_marker and (row.get("pointer_id"), row.get("enter_serial"),
                row.get("motion_ordinal"), row.get("time_ms")) in pointer_keys)
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
# Preserve exact selected outcomes even outside the bounded background window.
selected.extend(original for proof, original in lifecycle if proof in proofs
                or (palette_mode and original.partition("serial=")[2].split()[0] in palette_serials))
if not palette_mode:
    selected.extend(original for _, original in lifecycle[-128:])
selected_serials = {line.partition("serial=")[2].split()[0] for line in selected if line.startswith("FRAME-LIFECYCLE serial=")}
selected.extend(original for serial, original in submissions if serial in selected_serials)
selected_gpu = [(serial, original) for serial, original in gpu_submissions if serial in selected_serials]
if len({serial for serial, _ in selected_gpu}) != len(selected_gpu):
    raise SystemExit("Duplicate selected GPU submission identity")
selected.extend(original for _, original in selected_gpu)
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


def collect_settings_hover_trace(device, log_path, records, observer_session, after_input_id):
    """Read one exact owned-worker cohort after an explicit preparation boundary.

    The caller supplies an authenticated pinned Device adapter and revalidates
    its surface/worker/runtime. No launch, focus, motion or screenshot occurs here.
    """
    if type(after_input_id) is not int or after_input_id < 0:
        raise ValueError("An explicit Settings input boundary is required")
    raw = device.execute(["/usr/bin/python3", "-c", READ_TRACE_SCRIPT, log_path,
                          "settings.nav", observer_session, "so.tilo.settings", str(after_input_id)])
    for line in raw.splitlines():
        if line:
            records.setdefault(line, None)
    return list(records)


def collect_palette_trace(device, log_path, records, process_id, observer_session,
                          after_input_id, settings_process_id, settings_observer_session):
    """Read content-free palette and exact Settings invocation/presenter witnesses.

    The caller must pin and revalidate both authenticated producer lifetimes.
    This collector does not launch, focus, inject input, or take screenshots.
    """
    for value in (process_id, settings_process_id):
        if type(value) is not int or not 0 < value < 2**64:
            raise ValueError("Explicit producer process identity required")
    if type(after_input_id) is not int or not 0 <= after_input_id < 2**64:
        raise ValueError("Explicit palette input boundary required")
    raw = device.execute(["/usr/bin/python3", "-c", READ_TRACE_SCRIPT, log_path,
                          "palette", observer_session, "palette", str(after_input_id),
                          str(process_id), str(settings_process_id), settings_observer_session])
    for line in raw.splitlines():
        if line:
            records.setdefault(line, None)
    return list(records)
