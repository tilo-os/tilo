"""Native input/CPU/worker delivery diagnostics, never display certification."""
import json
from runtime_benchmark import correlate_frames, distribution


def rows(lines, prefix):
    return [json.loads(line[len(prefix):]) for line in lines if line.startswith(prefix)]


def summarize_native_delivery(scenario, lines, frames):
    expected = set(range(len(scenario["steps"])))
    warmup = scenario.get("warmup_steps", 0)
    handled = rows(lines, "tilo-native-input ")
    seen = set()
    lateness = []
    scheduler_lateness = []
    queue_delay = []
    sent_presence = ["sent_us" in row for row in handled]
    if any(sent_presence) and not all(sent_presence):
        raise ValueError("Partial scheduler-send timing evidence")
    for position, row in enumerate(handled):
        identity = row["id"]
        if type(identity) is not int or identity not in expected or identity in seen or identity != position:
            raise ValueError("Invalid or duplicate native input identity")
        seen.add(identity)
        if any(type(row.get(key)) is not int or row[key] < 0
               for key in ["scheduled_us", "dispatch_start_us", "lateness_us"]):
            raise ValueError("Invalid native input timing")
        if (row["scheduled_us"] != identity * scenario["interval_us"]
                or row["dispatch_start_us"] < row["scheduled_us"]
                or type(row["warmup"]) is not bool
                or row["warmup"] != (identity < warmup)
                or row["input"] != scenario["steps"][identity]
                or row["lateness_us"] != max(0, row["dispatch_start_us"] - row["scheduled_us"])):
            raise ValueError("Native input differs from requested schedule or payload")
        if "sent_us" in row:
            sent = row["sent_us"]
            if type(sent) is not int or not row["scheduled_us"] <= sent <= row["dispatch_start_us"]:
                raise ValueError("Invalid scheduler-send timestamp")
            if identity >= warmup:
                scheduler_lateness.append(sent - row["scheduled_us"])
                queue_delay.append(row["dispatch_start_us"] - sent)
        if identity >= warmup:
            lateness.append(row["lateness_us"])
    if seen != expected:
        raise ValueError("Scheduled native inputs were not all dispatched")
    # Keep actual cadence distinct from the intended timeline. A sender can
    # arrive late then catch up, even when every deadline is correctly fixed.
    for previous, current in zip(handled, handled[1:]):
        for key in (['dispatch_start_us', 'sent_us'] if all(sent_presence)
                    else ['dispatch_start_us']):
            if current[key] < previous[key]:
                raise ValueError('Native input clock moved backwards')
    measured_inputs = [row for row in handled if row['id'] >= warmup]
    def intervals(key):
        return distribution([current[key] - previous[key]
                             for previous, current in zip(measured_inputs, measured_inputs[1:])],
                            scale=0.001)
    cadence = {
        'scheduled_intervals': intervals('scheduled_us'),
        'sent_intervals': intervals('sent_us') if all(sent_presence) else None,
        'dispatch_intervals': intervals('dispatch_start_us'),
        'clock': 'Native input monotonic clock; these are arrival intervals, not display latency.',
    }
    mapped = set()
    frame_ids = set()
    measured_ids = set()
    mixed = []
    coalesced = 0
    for row in rows(lines, "tilo-native-frame "):
        frame = row["cpu_frame_id"]
        inputs = row["input_ids"]
        if type(frame) is not int or frame < 0 or frame in frame_ids or not inputs:
            raise ValueError("Invalid native frame mapping")
        frame_ids.add(frame)
        for identity in inputs:
            if type(identity) is not int or identity not in expected or identity in mapped:
                raise ValueError("Missing or duplicate native input/frame mapping")
            mapped.add(identity)
        if len(inputs) > 1:
            coalesced += 1
        if min(inputs) < warmup <= max(inputs):
            mixed.append(frame)
        elif min(inputs) >= warmup:
            measured_ids.add(frame)
    if mapped != expected:
        raise ValueError("Not all native inputs reached a CPU frame")
    if any(type(row.get("total_us")) is not int or row["total_us"] < 0 for row in frames):
        raise ValueError("Invalid CPU frame timing")
    all_cpu_ids = {row["frame"] for row in frames}
    if not frame_ids <= all_cpu_ids:
        raise ValueError("Native frame mapping has no CPU sample")
    measured = [row for row in frames if row["frame"] in measured_ids]
    phases = input_phase_summary(frames, measured)
    return {
        "performance_certified": False,
        "effects_validated": False,
        "measured_inputs": len(lateness),
        "input_cadence": cadence,
        "measured_cpu_frames": len(measured),
        "coalesced_frame_batches": coalesced,
        "mixed_warmup_frame_ids": mixed,
        "input_lateness": distribution(lateness, scale=0.001),
        "scheduler_lateness": distribution(scheduler_lateness, scale=0.001) if scheduler_lateness else None,
        "delivery_to_dispatch": distribution(queue_delay, scale=0.001) if queue_delay else None,
        "active_cpu_frame_build": distribution([row["total_us"] for row in measured], scale=0.001),
        "input_phases": phases,
        "frame_correlation": correlate_frames(frames, lines),
        "active_frame_correlation": correlate_frames(measured, [
            "tilo-render-frame " + json.dumps(row)
            for row in rows(lines, "tilo-render-frame ") if row.get("cpu_frame_id") in measured_ids]),
        "limitations": ["Input identity proves dispatch and frame association, not an observable effect.",
                        "Coalesced inputs are not extra frames; mixed warmup frames are excluded.",
                        "CPU/worker evidence is not GPU completion or display cadence."],
    }


INPUT_PHASE_KEYS = ["input_dispatch_us", "input_hit_test_us", "input_resources_us",
                    "input_publication_us", "input_commit_us"]


def input_phase_summary(frames, measured):
    """Measured frames' banked input split into events and phases; None for old evidence.

    `handle_event_us` sums every event since the previous frame; the per-event
    maximum and the phases say what one event and each stage cost. None of
    these are presentation latency. Every CPU frame of the stream must carry
    the fields, or none may.
    """
    present = ["input_events" in row for row in frames]
    if not any(present):
        return None
    if not all(present):
        raise ValueError("Partial input phase evidence")
    keys = ["handle_event_us", "input_events", "input_event_max_us", "input_to_commit_us",
            *INPUT_PHASE_KEYS]
    for row in frames:
        if any(type(row.get(key)) is not int or row[key] < 0 for key in keys):
            raise ValueError("Invalid input phase timing")
        if (sum(row[key] for key in INPUT_PHASE_KEYS) > row["handle_event_us"]
                or row["input_event_max_us"] > row["handle_event_us"]):
            raise ValueError("Input phases exceed the banked event time")
    frames = measured
    events = sorted(row["input_events"] for row in frames)
    return {
        "events_per_frame": {"n": len(events), "min": events[0],
                             "median": events[(len(events) - 1) // 2], "max": events[-1]},
        "banked_handle_event": distribution([row["handle_event_us"] for row in frames], scale=0.001),
        "costliest_event": distribution([row["input_event_max_us"] for row in frames], scale=0.001),
        "oldest_input_to_cpu_commit": distribution([row["input_to_commit_us"] for row in frames],
                                                   scale=0.001),
        "phases": {key[len("input_"):-len("_us")]: distribution([row[key] for row in frames], scale=0.001)
                   for key in INPUT_PHASE_KEYS},
        "limitations": ["Banked sums cover every event since the previous frame, not one event.",
                        "CPU commit precedes render submission and presentation."],
    }


def validate_scroll_effects(scenario, lines):
    """Require independently observed, correctly directed movement for each wheel.

    This validates widget state, not pixels, GPU completion, or presentation.
    """
    import math
    effects = rows(lines, "tilo-native-effect ")
    steps = scenario["steps"]
    if len(effects) != len(steps):
        raise ValueError("Missing or extra native effect snapshots")
    moved = 0
    for identity, (step, effect) in enumerate(zip(steps, effects)):
        if type(effect.get("id")) is not int or effect["id"] != identity:
            raise ValueError("Invalid or reordered native effect identity")
        if any(not isinstance(effect.get(key), dict) for key in ("before", "after")):
            raise ValueError("Missing native effect state snapshot")
        before = effect["before"].get("offset_y")
        after = effect.get("after", {}).get("offset_y")
        if any(type(value) not in (int, float) or not math.isfinite(value)
               for value in (before, after)):
            raise ValueError("Invalid observed scroll offset")
        if step["kind"] == "wheel":
            dy = step.get("dy")
            if type(dy) not in (int, float) or not math.isfinite(dy) or dy == 0:
                raise ValueError("Scroll workload requires a nonzero vertical wheel delta")
            if (after - before) * dy >= 0:
                raise ValueError("Native wheel did not move content in the requested direction")
            moved += 1
    if not moved:
        raise ValueError("Scroll workload contains no wheel inputs")
    return moved


def validate_frame_scroll_states(scenario, lines):
    """Check post-draw scroll state against the last input effect in every batch.

    Standalone mapping validation is included. Call summarize_native_delivery
    separately to verify CPU samples and worker association. This proves state
    survived frame construction, not that the state was visibly presented.
    """
    import math

    def finite_number(value):
        return type(value) in (int, float) and math.isfinite(value)

    effects = rows(lines, "tilo-native-effect ")
    count = len(scenario["steps"])
    if not count or len(effects) != count:
        raise ValueError("Missing or extra native effect snapshots")
    for identity, effect in enumerate(effects):
        after = effect.get("after")
        if (type(effect.get("id")) is not int or effect["id"] != identity
                or not isinstance(after, dict) or not finite_number(after.get("offset_y"))):
            raise ValueError("Invalid or reordered native effect state")

    frames = rows(lines, "tilo-native-frame ")
    next_input = 0
    last_frame = -1
    offsets, maxima, windows, block_counts = [], [], [], []
    measured = mixed = 0
    warmup = scenario.get("warmup_steps", 0)
    window_keys = ("body_window_start", "body_window_len", "body_block_count")
    window_mode = None
    for row in frames:
        frame, inputs, state = row.get("cpu_frame_id"), row.get("input_ids"), row.get("state")
        if (type(frame) is not int or frame <= last_frame or not isinstance(inputs, list)
                or not inputs or any(type(identity) is not int for identity in inputs)
                or inputs != list(range(next_input, next_input + len(inputs)))
                or next_input + len(inputs) > count):
            raise ValueError("Missing, duplicate, or reordered native frame mapping")
        last_frame = frame
        next_input += len(inputs)
        if not isinstance(state, dict):
            raise ValueError("Missing post-draw scroll state")
        offset, maximum = state.get("offset_y"), state.get("max_scroll")
        if (not finite_number(offset) or not finite_number(maximum)
                or not 0 <= offset <= maximum):
            raise ValueError("Post-draw scroll state is invalid or out of bounds")
        if offset != effects[inputs[-1]]["after"]["offset_y"]:
            raise ValueError("Last input scroll effect did not survive frame construction")
        offsets.append(offset)
        maxima.append(maximum)
        if inputs[0] >= warmup:
            measured += 1
        elif inputs[-1] >= warmup:
            mixed += 1
        present = [key in state for key in window_keys]
        if any(present) and not all(present):
            raise ValueError("Partial body window state")
        if window_mode is not None and window_mode != all(present):
            raise ValueError("Missing body window state in some frames")
        window_mode = all(present)
        if window_mode:
            start, length, blocks = (state[key] for key in window_keys)
            if (any(type(value) is not int or value < 0 for value in (start, length, blocks))
                    or start + length > blocks):
                raise ValueError("Invalid body window bounds")
            windows.append((start, start + length))
            block_counts.append(blocks)
    if next_input != count:
        raise ValueError("Not all native inputs reached a post-draw frame state")
    coverage = None
    if windows:
        # Interval union avoids allocating one element per document block.
        distinct, end = 0, 0
        for start, stop in sorted(windows):
            distinct += max(0, stop - max(start, end))
            end = max(end, stop)
        coverage = {
            "distinct_observed_blocks": distinct,
            "window_start_range": [min(start for start, _ in windows), max(start for start, _ in windows)],
            "block_count_range": [min(block_counts), max(block_counts)],
            "stable_block_count": len(set(block_counts)) == 1,
        }
    return {
        "performance_certified": False,
        "validated_frames": len(frames),
        "validated_inputs": count,
        "measured_frames": measured,
        "mixed_warmup_frames": mixed,
        "offset_y_range": [min(offsets), max(offsets)],
        "max_scroll_range": [min(maxima), max(maxima)],
        "body_window_coverage": coverage,
        "limitations": ["Post-draw widget state is not pixel or presentation evidence.",
                        "Block coverage counts observed index ranges, not document content identity."],
    }
