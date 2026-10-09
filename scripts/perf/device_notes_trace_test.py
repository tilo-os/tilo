"""Exercise the production collector against a rolling, oversized flight file."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from device_notes_trace import collect_trace
from input_latency import summarize_physical_notes_latency


class LocalSlot:
    def __init__(self):
        self.output_sizes = []

    def execute(self, arguments):
        result = subprocess.run(arguments, text=True, capture_output=True, check=True)
        self.output_sizes.append(len(result.stdout.encode()))
        if self.output_sizes[-1] > 1048576:
            raise RuntimeError("slot exec output limit")
        return result.stdout


def sample(number, render_phases=False):
    page = "new-scratch-page"
    session = "2bd78136-4c17-4853-9d7c-7442274a7059"
    worker = dict(process_id=123, observer_session=session)
    def state(count):
        digest = f"{count:064x}"
        return dict(app="so.tilo.notes", selected_page=page, main_page=page,
                    editor_binding_page=page, editor_focused=True,
                    hash_algorithm="blake3", body_hash=digest, rendered_body_hash=digest,
                    body_chars=count, body_bytes=count, rendered_body_chars=count,
                    caret=dict(byte_offset=count))
    start = number * 100_000_000
    records = [
        ("tilo-physical-delivery ", dict(wayland_serial=number + 9, surface_id=42,
            clock_id=1, delivery_time_ns=start + 1_000_000, input_time_ns=start)),
        ("tilo-physical-input ", dict(**worker, id=number, kind="key_pressed",
            wayland_serial=number + 9, before=state(number - 1), after=state(number),
            clock="clock_monotonic_seconds", dispatch_lo_s=(start + 2_000_000)/1e9,
            dispatch_hi_s=(start + 2_100_000)/1e9)),
        ("tilo-physical-frame ", dict(**worker, cpu_frame_id=number,
            input_ids=[number], state=state(number))),
        ("tilo-display-frame ", dict(**worker, backend="wayland", cpu_frame_id=number,
            clock_id=1, flags=7, discarded=False, presented_time_ns=start + 10_000_000, sequence=number)),
    ]
    if render_phases:
        for tag in ("tilo-native-send", "tilo-native-main-stages", "tilo-native-worker",
                    "tilo-native-worker-handoff", "tilo-native-present-call"):
            records.append((tag + " ", dict(**worker, cpu_frame_id=number,
                host_clock="clock_monotonic_seconds", before_host_lo_s=start / 1e9)))
    return "\n".join(tag + json.dumps(row) for tag, row in records)


class RollingCollection(unittest.TestCase):
    def test_render_phases_keep_exact_frame_and_observer_lifetime(self):
        slot = LocalSlot()
        captured = {}
        identity = dict(process_id=123,
                        observer_session="2bd78136-4c17-4853-9d7c-7442274a7059",
                        cpu_frame_id=1)
        tags = ("tilo-native-send", "tilo-native-main-stages", "tilo-native-worker",
                "tilo-native-worker-handoff", "tilo-native-present-call",
                "tilo-native-gpu-stages")
        rows = []
        for tag in tags:
            row = dict(identity, host_clock="clock_monotonic_seconds", before_host_lo_s=0.102)
            rows.append(tag + " " + json.dumps(row))
            for changes in (dict(observer_session="another-sandbox-lifetime"),
                            dict(process_id=456), dict(cpu_frame_id=2), dict(cpu_frame_id=None)):
                rows.append(tag + " " + json.dumps(dict(row, **changes)))
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            log.write_text(sample(1) + "\n" + "\n".join(rows))
            collect_trace(slot, str(log), "new-scratch-page", captured)
        phases = [line for line in captured if line.startswith("tilo-native-")]
        self.assertEqual(len(phases), len(tags))
        for line in phases:
            row = json.loads(line.split(" ", 1)[1])
            for key, value in identity.items():
                self.assertEqual(row[key], value)
        report = summarize_physical_notes_latency(list(captured), "new-scratch-page")
        self.assertEqual(len(report["samples"]), 1)
        self.assertTrue(report["latency_complete"])

    def test_exact_capture_witnesses_are_selected_and_arbitrary_fields_are_not_exported(self):
        from physical_notes_stages_test import ForeignCaptureJoin
        fixture = ForeignCaptureJoin().fixture()
        for tag, row in fixture:
            row['not_permitted_user_text'] = 'must never leave the device'
            if row.get('event') == 'imported':
                row.update(copy_encoder_finish_return_ns=16500, fence_ready=True,
                           fence_poll_calls=1, fence_poll_events=1)
            if row.get('event') == 'completed':
                row['physical_presented_time_ns'] = 110000000
                row['physical_sequence'] = 1
        log_rows = [tag + json.dumps(row) for tag, row in fixture]
        log_rows.append('tilo-present-pipe ' + json.dumps(dict(event='retired', clock_id=1,
            pipe_serial=999, enqueue_ns=1, render_start_ns=2, render_end_ns=3)))
        captured = {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / 'latest.log'
            log.write_text(sample(1) + '\n' + '\n'.join(log_rows))
            collect_trace(LocalSlot(), str(log), 'new-scratch-page', captured)
        diagnostics = [line for line in captured if line.startswith(('tilo-foreign-capture ',
                       'tilo-compositor-feedback ', 'tilo-present-pipe '))]
        self.assertEqual(len(diagnostics), 5)
        self.assertNotIn('must never leave', '\n'.join(diagnostics))
        self.assertNotIn('not_permitted', '\n'.join(diagnostics))
        imported = next(json.loads(line.split(' ', 1)[1]) for line in diagnostics
                        if json.loads(line.split(' ', 1)[1]).get('event') == 'imported')
        self.assertEqual(imported['copy_encoder_finish_return_ns'], 16500)
        self.assertTrue(imported['fence_ready'])
        self.assertEqual(imported['fence_poll_calls'], 1)
        self.assertFalse(any(json.loads(line.split(' ', 1)[1]).get('pipe_serial') == 999
                             for line in diagnostics))

    def test_complete_hundred_inputs_survive_rollover_and_the_rpc_limit(self):
        slot = LocalSlot()
        captured = {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            previous = ""
            for number in range(1, 101):
                current = sample(number, render_phases=True)
                # Replace the retained file, keeping only two input cohorts.
                # Unrelated renderer output alone exceeds slot exec's1MiB cap.
                log.write_text("renderer diagnostic " + "x" * 4194304 + "\n"
                               + previous + "\n" + current)
                collect_trace(slot, str(log), "new-scratch-page", captured)
                previous = current
        report = summarize_physical_notes_latency(list(captured), "new-scratch-page")
        self.assertEqual(len(report["samples"]), 100)
        self.assertEqual([row["input_id"] for row in report["samples"]], list(range(1, 101)))
        self.assertTrue(report["latency_complete"])
        self.assertEqual(sum(line.startswith("tilo-native-") for line in captured), 500)
        self.assertLess(max(slot.output_sizes), 1048576)

    def test_repeated_witness_frames_are_collected_incrementally_within_bound(self):
        slot, captured = LocalSlot(), {}
        cohorts = []
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            for number in range(1, 5):
                base = sample(number, render_phases=True).splitlines()
                cohort = [line for line in base if line.startswith(
                    ("tilo-physical-input ", "tilo-physical-delivery "))]
                for index in range(32):
                    for line in base:
                        tag, value = line.split(" ", 1)
                        row = json.loads(value)
                        if "cpu_frame_id" not in row:
                            continue
                        row["cpu_frame_id"] = number * 100 + index
                        if tag.startswith("tilo-native-"):
                            row["fixture_stage_fields"] = "x" * 1900
                        cohort.append(tag + " " + json.dumps(row))
                cohorts.extend(cohort)
                log.write_text("\n".join(cohorts))
                collect_trace(slot, str(log), "new-scratch-page", captured)
        report = summarize_physical_notes_latency(list(captured), "new-scratch-page")
        self.assertEqual(len(report["samples"]), 4)
        self.assertEqual(sum(line.startswith("tilo-physical-frame ")
                             for line in captured), 128)
        self.assertLess(max(slot.output_sizes), 524288)

    def test_same_outcome_phases_survive_background_window_without_private_text(self):
        from physical_notes_stages_test import CompositorPhaseJoin
        phase = CompositorPhaseJoin()
        exact = phase.line(physical_presented_time_ns=110000000, physical_sequence=1)
        background = [phase.line(serial=n, physical_sequence=n,
                                 physical_presented_time_ns=200000000 + n) for n in range(42, 192)]
        slot, captured = LocalSlot(), {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            log.write_text(sample(1) + "\n" + exact + " password=private-do-not-copy\n" + "\n".join(background))
            collect_trace(slot, str(log), "new-scratch-page", captured)
        frames = [line for line in captured if line.startswith('FRAME-LIFECYCLE ')]
        self.assertEqual(len(frames), 129)
        self.assertIn(exact, frames)
        self.assertNotIn('private-do-not-copy', '\n'.join(captured))
        self.assertLess(max(slot.output_sizes), 524288)

    def test_submission_boundaries_keep_only_exact_selected_serials_and_safe_fields(self):
        from compositor_submission_test import record
        from physical_notes_stages_test import CompositorPhaseJoin
        lifecycle = CompositorPhaseJoin().line()
        slot, captured = LocalSlot(), {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            log.write_text(sample(1) + "\n" + lifecycle + "\n" + record()
                           + " private=do-not-export\n" + record(900))
            collect_trace(slot, str(log), "new-scratch-page", captured)
        self.assertIn(record(), captured)
        self.assertNotIn(record(900), captured)
        self.assertNotIn('do-not-export', '\n'.join(captured))

    def test_conflicting_duplicate_input_is_preserved_and_rejected(self):
        slot = LocalSlot()
        captured = {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            log.write_text(sample(1))
            collect_trace(slot, str(log), "new-scratch-page", captured)
            log.write_text(sample(1).replace('"dispatch_hi_s": 0.1021',
                                            '"dispatch_hi_s": 0.1022'))
            collect_trace(slot, str(log), "new-scratch-page", captured)
        with self.assertRaisesRegex(ValueError, "duplicate physical input"):
            summarize_physical_notes_latency(list(captured), "new-scratch-page")


class SettingsHoverCollection(unittest.TestCase):
    def test_production_reader_scopes_worker_boundary_and_whitelists_pointer_metadata(self):
        from device_notes_trace import collect_settings_hover_trace
        from physical_notes_latency_test import PhysicalSettingsHoverLatency
        rows = PhysicalSettingsHoverLatency().timeline()
        rows[0][1]['private_value'] = 'do-not-export'
        unknown = json.loads(json.dumps(rows[1][1]))
        unknown['observer_session'] = '13d78136-4c17-4853-9d7c-7442274a7059'
        slot = LocalSlot()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'flight.log'
            path.write_text('\n'.join(prefix+json.dumps(row) for prefix,row in rows)
                            +'\n'+'tilo-physical-input '+json.dumps(unknown))
            captured = collect_settings_hover_trace(slot,str(path),{},rows[1][1]['observer_session'],0)
            self.assertNotIn('do-not-export','\n'.join(captured))
            self.assertNotIn(unknown['observer_session'],'\n'.join(captured))
            self.assertTrue(PhysicalSettingsHoverLatency().report([
                (line[:line.index(' ')+1],json.loads(line[line.index(' ')+1:])) for line in captured
            ])['latency_complete'])
            self.assertEqual(collect_settings_hover_trace(slot,str(path),{},rows[1][1]['observer_session'],1),[])
            with self.assertRaises(ValueError):
                collect_settings_hover_trace(slot,str(path),{},rows[1][1]['observer_session'],None)
            with self.assertRaises(subprocess.CalledProcessError):
                collect_settings_hover_trace(slot,str(path),{},'unrelated',0)



class PaletteCollection(unittest.TestCase):
    session = "2bd78136-4c17-4853-9d7c-7442274a7059"
    settings_session = "abcdef12-1234-4567-8234-123456789abc"
    invocation = "12345678-1234-4567-8234-123456789abc"

    def rows(self, click=False):
        def state():
            return dict(visible=True, mounted=True, query_hash="a" * 64, query_bytes=9,
                results_generation=7, query_current=True, published_revision_hash="b" * 64,
                selected_entry_id="result:1", hovered_entry_id=None, bounds=[0,0,200,200],
                query_bounds=[0,0,200,30], query="private raw query", rendered={"private":"secret"},
                rows=[dict(entry_id="result:1", provider_id="settings", action_id="open-bluetooth",
                           bounds=[0,30,200,60], visible_index=0,result_index=1,
                           selected=True,hovered=False,label="private row label")])
        owner = dict(schema=1,clock_id=1,process_id=42,observer_session=self.session)
        rows = [("tilo-palette-input ",dict(owner,id=1,kind="physical_key",input_time_ns=100,
            dispatch_start_ns=200,dispatch_end_ns=300,before=state(),after=state()))]
        if not click:
            rows.append(("tilo-palette-frame ",dict(owner,pipe_serial=41,scene_revision=9,
                input_ids=[1],draw_time_ns=400,submit_time_ns=500,state=state())))
        else:
            worker = dict(process_id=2,observer_session=self.settings_session)
            nav = dict(selected_pane="bluetooth",requested_pane="bluetooth",path="settings://bluetooth",
                ready=True,pending=False,content_visible=True,private="must stay local")
            snapshot = dict(app="so.tilo.settings",navigation=nav,rendered_navigation=nav)
            rows.extend([
                ("tilo-action-submission ",dict(process_id=42,invocation=self.invocation,
                    application="so.tilo.settings",source="physical_key",clock_id=1,input_time_ns=100,
                    activation_dispatch_ns=200,submission_time_ns=250,palette_input_id=1,
                    palette_observer_session=self.session,palette_target_provider="settings",
                    palette_target_action="open-bluetooth",query="private raw query")),
                ("tilo-broker-input ",dict(worker,invocation=self.invocation,
                    clock="clock_monotonic_seconds",handler_returned_ok=True,
                    handler_dispatch_lo_s=.001,handler_dispatch_hi_s=.002,
                    handler_return_lo_s=.003,handler_return_hi_s=.004,before=snapshot,after=snapshot)),
                ("tilo-broker-frame ",dict(worker,invocations=[self.invocation],cpu_frame_id=17,state=snapshot)),
                ("tilo-display-frame ",dict(worker,backend="wayland",cpu_frame_id=17,
                    clock_id=1,flags=7,discarded=False,presented_time_ns=600,sequence=5,private="secret")),
            ])
        return rows

    def collect(self, rows, boundary=0):
        from device_notes_trace import collect_palette_trace
        from compositor_submission_test import gpu_record,record
        from physical_notes_stages_test import CompositorPhaseJoin
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            text = "\n".join(tag+json.dumps(row) for tag,row in rows)
            phase = CompositorPhaseJoin().line(revision=9,physical_presented_time_ns=600,physical_sequence=5)
            log.write_text(text+"\n"+phase+"\n"+record()+"\n"+gpu_record()+
                "\n"+CompositorPhaseJoin().line(serial=999,physical_sequence=999)+"\n"+gpu_record(serial=999,first_recording=99,last_recording=99))
            return collect_palette_trace(LocalSlot(),str(log),{},42,self.session,boundary,2,self.settings_session)

    def test_content_free_snapshot_and_exact_independent_presenter_records(self):
        rows = self.rows()
        foreign = [(tag,dict(row,process_id=999)) for tag,row in rows]
        result = self.collect(rows+foreign)
        joined = "\n".join(result)
        for private in ("private raw query","private row label","secret","rendered", "serial=999"):
            self.assertNotIn(private,joined)
        for marker in ("tilo-palette-input ","tilo-palette-frame ","FRAME-LIFECYCLE ",
                       "FRAME-SUBMISSION ","FRAME-GPU-SUBMISSION "):
            self.assertEqual(sum(line.startswith(marker) for line in result),1)
        self.assertEqual(self.collect(rows,boundary=1),[])

    def test_exact_invocation_and_settings_worker_preserve_real_presentation(self):
        rows=self.rows(True)
        foreign=[(tag,dict(row,process_id=999)) for tag,row in rows if tag.startswith("tilo-broker") or tag=="tilo-display-frame "]
        result=self.collect(rows+foreign)
        for marker in ("tilo-action-submission ","tilo-broker-input ","tilo-broker-frame ","tilo-display-frame "):
            self.assertEqual(sum(line.startswith(marker) for line in result),1)
        self.assertNotIn("private", "\n".join(result))
        self.assertTrue(any(line.startswith("FRAME-GPU-SUBMISSION serial=41 ") for line in result))
        # Wrong actual worker cannot provide a frame or a presentation witness.
        wrong=[(tag,dict(row,process_id=999)) if tag.startswith("tilo-broker") or tag=="tilo-display-frame " else (tag,row) for tag,row in rows]
        captured=self.collect(wrong)
        self.assertFalse(any(line.startswith(("tilo-broker-frame ","tilo-display-frame ","FRAME-LIFECYCLE ")) for line in captured))

    def test_current_event_family_does_not_export_historical_or_nearby_frames(self):
        import copy
        rows = self.rows()
        later = copy.deepcopy(rows)
        later[0][1]["id"] = 2
        later[1][1].update(input_ids=[2],pipe_serial=42)
        captured = self.collect(rows + later)
        events = [json.loads(line.split(" ",1)[1]) for line in captured
                  if line.startswith("tilo-palette-input ")]
        self.assertEqual([event["id"] for event in events],[2])
        frames = [json.loads(line.split(" ",1)[1]) for line in captured
                  if line.startswith("tilo-palette-frame ")]
        self.assertEqual([frame["pipe_serial"] for frame in frames],[42])
        # Serial 41 has a real retirement, but cannot substitute for serial 42.
        self.assertFalse(any(line.startswith(("FRAME-LIFECYCLE ","FRAME-GPU-SUBMISSION "))
                             for line in captured))

    def test_actual_unit_separator_semantic_addresses_roundtrip_without_query_ids(self):
        ids = ("directory\x1f42\x1f" + "a" * 64 + "\x1fsettings.pane.bluetooth\x1fopen",
               "so.tilo.settings\x1fread\x1froom-1\x1frecord-1\x1fsettings.pane.bluetooth\x1fopen")
        for address in ids:
            with self.subTest(address=address):
                rows = self.rows(True)
                for tag,row in rows:
                    for key in ("before","after","state"):
                        for entry in row.get(key,{}).get("rows",[]):
                            entry["action_id"] = address
                    if tag == "tilo-action-submission ":
                        row["palette_target_action"] = address
                result = self.collect(rows)
                action = next(json.loads(line.split(" ",1)[1]) for line in result
                              if line.startswith("tilo-action-submission "))
                self.assertEqual(action["palette_target_action"], address)
                event = next(json.loads(line.split(" ",1)[1]) for line in result
                             if line.startswith("tilo-palette-input "))
                self.assertEqual(event["before"]["rows"][0]["action_id"], address)
        rows = self.rows()
        rows[0][1]["before"]["rows"][0]["action_id"] = "live:search\x1fprivate needle\x1frest"
        with self.assertRaises(subprocess.CalledProcessError):
            self.collect(rows)

    def test_invalid_digest_geometry_and_missing_explicit_scope_fail(self):
        import copy
        from device_notes_trace import collect_palette_trace
        for mutate in (lambda state: state.update(query_hash="raw-query"),
                       lambda state: state.update(rows="private"),
                       lambda state: state.update(bounds=[0,float("nan"),2,3])):
            rows=copy.deepcopy(self.rows());mutate(rows[0][1]["after"])
            with self.assertRaises(subprocess.CalledProcessError):self.collect(rows)
        for process,boundary in ((0,0),(True,0),(42,None),(42,-1)):
            with self.assertRaises(ValueError):
                collect_palette_trace(LocalSlot(),"unused",{},process,self.session,boundary,2,self.settings_session)

if __name__ == "__main__":
    unittest.main()
