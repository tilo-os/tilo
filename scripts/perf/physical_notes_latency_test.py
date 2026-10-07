"""The existing latency analyser must reject unrelated or incomplete timelines."""
import copy
import json
import unittest
from input_latency import summarize_physical_notes_latency


class PhysicalNotesLatency(unittest.TestCase):
    def timeline(self):
        page = 'scratch-only'
        def state(n):
            return dict(app='so.tilo.notes', selected_page=page, main_page=page,
                        editor_binding_page=page, editor_focused=True, hash_algorithm='blake3',
                        body_hash=str(n)*64, rendered_body_hash=str(n)*64,
                        body_chars=n, rendered_body_chars=n, body_bytes=n,
                        caret=dict(byte_offset=n))
        session = '2bd78136-4c17-4853-9d7c-7442274a7059'
        result = []
        for n in range(1, 4):
            start = n*100_000_000
            result += [
                ('tilo-physical-delivery ', dict(wayland_serial=n+9, surface_id=42,
                                                clock_id=1, delivery_time_ns=start+1_000_000, input_time_ns=start)),
                ('tilo-physical-input ', dict(id=n, kind='key_pressed', wayland_serial=n+9, process_id=123, observer_session=session,
                                             before=state(n-1), after=state(n), clock='clock_monotonic_seconds',
                                             dispatch_lo_s=(start+2_000_000)/1e9, dispatch_hi_s=(start+2_100_000)/1e9)),
                ('tilo-physical-frame ', dict(cpu_frame_id=n, input_ids=[n], state=state(n), process_id=123, observer_session=session)),
                ('tilo-display-frame ', dict(backend='wayland', cpu_frame_id=n, clock_id=1, process_id=123, observer_session=session,
                                            flags=7, discarded=False, presented_time_ns=start+10_000_000)),
            ]
        return result

    def report(self, rows):
        return summarize_physical_notes_latency([prefix+json.dumps(row) for prefix,row in rows], 'scratch-only')

    def test_exact_changed_body_hardware_flip_is_required(self):
        report = self.report(self.timeline())
        self.assertTrue(report['latency_complete'])
        self.assertEqual([r['latency_ms'] for r in report['samples']], [10, 10, 10])
        self.assertNotIn('performance_certified', report)
        # An unrelated background worker's same local frame ID is not this
        # scratch buffer's presentation, even with the same sandbox PID.
        # The app worker lifetime UUID disambiguates actual producers.
        timeline = self.timeline()+[('tilo-display-frame ', dict(backend='wayland',
                     cpu_frame_id=1, process_id=123, observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154', clock_id=1, flags=7,
                     discarded=False, presented_time_ns=1))]
        self.assertTrue(self.report(timeline)['latency_complete'])

    def test_discarded_buffer_needs_an_exact_drawn_and_presented_successor(self):
        timeline = copy.deepcopy(self.timeline()[:4])
        first_feedback = timeline[3][1]
        first_feedback.clear()
        first_feedback.update(backend='wayland', cpu_frame_id=1, process_id=123,
                              observer_session=timeline[1][1]['observer_session'], discarded=True)
        redraw = copy.deepcopy(timeline[2][1])
        redraw['cpu_frame_id'] = 5
        shown = copy.deepcopy(self.timeline()[3][1])
        shown['cpu_frame_id'] = 5
        timeline += [('tilo-physical-frame ', redraw), ('tilo-display-frame ', shown)]
        report = self.report(timeline)
        self.assertEqual(report['samples'][0]['cpu_frame_id'], 5)
        self.assertEqual(report['samples'][0]['latency_ms'], 10)
        for mutation in ('missing_feedback', 'different_body', 'no_positive_feedback'):
            with self.subTest(mutation=mutation):
                invalid = copy.deepcopy(timeline)
                if mutation == 'missing_feedback':
                    invalid.pop(3)
                elif mutation == 'different_body':
                    invalid[2][1]['state']['body_hash'] = 'f' * 64
                else:
                    invalid[-1][1]['discarded'] = True
                with self.assertRaises(ValueError):
                    self.report(invalid)

    def test_indirect_missing_coalesced_wrong_note_and_foreign_effects_fail(self):
        for prefix, change in [
            ('tilo-physical-delivery ', dict(clock_id=0)),
            ('tilo-physical-delivery ', dict(input_time_ns=None)),
            ('tilo-physical-delivery ', dict(input_time_ns=999_000_000)),
            ('tilo-physical-input ', dict(dispatch_lo_s=None)),
            ('tilo-physical-input ', dict(wayland_serial=None)),
            ('tilo-physical-input ', dict(observer_session=None)),
            ('tilo-physical-input ', dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154')),
            ('tilo-physical-frame ', dict(observer_session=None)),
            ('tilo-physical-frame ', dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154')),
            ('tilo-display-frame ', dict(observer_session=None)),
            ('tilo-display-frame ', dict(observer_session='d18a8ac3-c453-437f-b6d6-3fef3f242154')),
            ('tilo-physical-input ', dict(kind='ime_commit')),
            ('tilo-physical-input ', dict(after={})),
            ('tilo-physical-frame ', dict(input_ids=[1, 2])),
            ('tilo-physical-frame ', dict(cpu_frame_id=99)),
            ('tilo-physical-frame ', dict(process_id=999)),
            ('tilo-physical-frame ', dict(state={})),
            ('tilo-display-frame ', dict(discarded=True)),
            ('tilo-display-frame ', dict(flags=1)),
            ('tilo-display-frame ', dict(flags=None)),
            ('tilo-display-frame ', dict(presented_time_ns=1)),
        ]:
            with self.subTest(prefix=prefix, change=change):
                timeline = copy.deepcopy(self.timeline())
                next(row for p,row in timeline if p == prefix).update(change)
                with self.assertRaises(ValueError):
                    self.report(timeline)
        for prefix in ['tilo-physical-delivery ', 'tilo-physical-input ',
                       'tilo-physical-frame ', 'tilo-display-frame ']:
            with self.subTest(missing=prefix):
                with self.assertRaises(ValueError):
                    self.report([(p,r) for p,r in self.timeline() if p != prefix])
                with self.assertRaises(ValueError):
                    self.report(self.timeline()+[next((p,r) for p,r in self.timeline() if p == prefix)])



class PhysicalSettingsLatency(unittest.TestCase):
    def timeline(self):
        def state(pane):
            nav=dict(selected_pane=pane,requested_pane=pane,path='settings://'+pane,
                pending=False,ready=True,content_visible=True,narrow=False,sidebar_open=True)
            return dict(app='so.tilo.settings',navigation=nav,rendered_navigation=copy.deepcopy(nav))
        rows=PhysicalNotesLatency().timeline()
        for prefix,row in rows:
            if prefix=='tilo-physical-input ':
                row.update(before=state('network'),after=state('bluetooth'),input_route_admitted=True)
            elif prefix=='tilo-physical-frame ':row['state']=state('bluetooth')
        return rows

    def report(self,rows,pane='bluetooth'):
        from input_latency import summarize_physical_settings_latency
        return summarize_physical_settings_latency([prefix+json.dumps(row) for prefix,row in rows],pane)

    def test_same_trusted_frame_and_clock_join_proves_exact_changed_pane(self):
        report=self.report(self.timeline())
        self.assertEqual(len(report['samples']),3)
        self.assertEqual(report['samples'][0]['latency_ms'],10)
        self.assertIn('Settings pane',report['metric'])

    def test_absent_wrong_stale_or_unchanged_state_never_qualifies(self):
        for mutation in ('unchanged','wrong','stale','pending','hidden','denied','no_origin','other_worker',
                'no_feedback','no_native_key','other_effect'):
            rows=copy.deepcopy(self.timeline());event=rows[1][1];frame=rows[2][1];feedback=rows[3][1]
            if mutation=='unchanged':event['before']=copy.deepcopy(frame['state'])
            elif mutation=='wrong':frame['state']['navigation']['selected_pane']='displays'
            elif mutation=='stale':frame['state']['rendered_navigation']['selected_pane']='network'
            elif mutation=='pending':frame['state']['navigation']['pending']=True
            elif mutation=='hidden':frame['state']['navigation']['content_visible']=False
            elif mutation=='denied':event['input_route_admitted']=False
            elif mutation=='no_origin':rows[0][1]['input_time_ns']=None
            elif mutation=='other_worker':frame['observer_session']='13d78136-4c17-4853-9d7c-7442274a7059'
            elif mutation=='no_feedback':feedback['discarded']=True
            elif mutation=='no_native_key':event['wayland_serial']=None
            elif mutation=='other_effect':event['after']['navigation']['requested_pane']='displays'
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):self.report(rows)
        with self.assertRaises(ValueError):self.report(self.timeline(),'displays')

class PhysicalSettingsHoverLatency(unittest.TestCase):
    def timeline(self):
        result = PhysicalSettingsLatency().timeline()
        for prefix, row in result:
            if prefix == 'tilo-physical-delivery ':
                row.pop('wayland_serial')
                row.update(pointer_id=17, enter_serial=29, motion_ordinal=1, time_ms=31)
            elif prefix == 'tilo-physical-input ':
                row.update(kind='pointer_motion', wayland_serial=None, pointer_id=17,
                           pointer_enter_serial=29, pointer_motion_ordinal=1, pointer_time_ms=31)
                for name in ('before', 'after'):
                    state = row[name]
                    state['navigation']['selected_pane'] = 'network'
                    state['navigation']['requested_pane'] = 'network'
                    state['navigation']['path'] = 'settings://network'
                    state['rendered_navigation'] = copy.deepcopy(state['navigation'])
                    state['hover'] = dict(control='settings.nav',mounted=True,
                                          pane=None if name=='before' else 'bluetooth')
                    state['rendered_hover'] = copy.deepcopy(state['hover'])
            elif prefix == 'tilo-physical-frame ':
                state = row['state']
                state['navigation'].update(selected_pane='network',requested_pane='network',path='settings://network')
                state['rendered_navigation'] = copy.deepcopy(state['navigation'])
                state['hover'] = dict(control='settings.nav',mounted=True,pane='bluetooth')
                state['rendered_hover'] = copy.deepcopy(state['hover'])
        result = [('tilo-physical-pointer-delivery ' if prefix=='tilo-physical-delivery ' else prefix,row)
                  for prefix,row in result[:4]]
        return result

    def report(self, rows):
        from input_latency import summarize_physical_settings_hover_latency
        return summarize_physical_settings_hover_latency([prefix+json.dumps(row) for prefix,row in rows], 'bluetooth')

    def test_exact_motion_and_enter_use_existing_trusted_frame_join(self):
        report = self.report(self.timeline())
        self.assertEqual(report['samples'][0]['latency_ms'],10)
        self.assertNotIn('wayland_serial',report['samples'][0])
        self.assertEqual(report['samples'][0]['pointer_motion_ordinal'],1)
        enter = copy.deepcopy(self.timeline())
        enter[0][1].update(motion_ordinal=0,time_ms=None)
        enter[1][1].update(pointer_motion_ordinal=0,pointer_time_ms=None)
        self.assertEqual(self.report(enter)['samples'][0]['latency_ms'],10)

    def test_wrong_wire_identity_stale_state_missing_origin_or_feedback_refuse(self):
        for mutation in ('wrong_pointer','wrong_enter','wrong_ordinal','wrong_time','late_bind',
                         'duplicate','no_origin','bad_clock','synthetic','coalesced','stale',
                         'unchanged','selection','hidden','wrong_control','denied','no_feedback',
                         'other_worker','missing_delivery','missing_draw'):
            rows = copy.deepcopy(self.timeline())
            delivery,event,frame,feedback = [row for _,row in rows]
            if mutation=='wrong_pointer':delivery['pointer_id']=18
            elif mutation=='wrong_enter':delivery['enter_serial']=30
            elif mutation=='wrong_ordinal':delivery['motion_ordinal']=2
            elif mutation=='wrong_time':delivery['time_ms']=32
            elif mutation=='late_bind':event['pointer_enter_serial']=None
            elif mutation=='duplicate':rows.append(copy.deepcopy(rows[0]))
            elif mutation=='no_origin':delivery['input_time_ns']=None
            elif mutation=='bad_clock':delivery['clock_id']=0
            elif mutation=='synthetic':event['kind']='key_pressed'
            elif mutation=='coalesced':frame['input_ids']=[1,2]
            elif mutation=='stale':frame['state']['rendered_hover']['pane']=None
            elif mutation=='unchanged':event['before']['hover']['pane']='bluetooth'
            elif mutation=='selection':frame['state']['navigation']['selected_pane']='bluetooth'
            elif mutation=='hidden':frame['state']['hover']['mounted']=False
            elif mutation=='wrong_control':frame['state']['hover']['control']='private'
            elif mutation=='denied':event['input_route_admitted']=False
            elif mutation=='no_feedback':feedback['discarded']=True
            elif mutation=='other_worker':frame['observer_session']='13d78136-4c17-4853-9d7c-7442274a7059'
            elif mutation=='missing_delivery':rows.pop(0)
            elif mutation=='missing_draw':rows.pop(2)
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):self.report(rows)


if __name__ == '__main__':
    unittest.main()
