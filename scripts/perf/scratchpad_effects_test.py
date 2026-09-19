import copy
import json
import unittest
from scratchpad_effects import validate_scratchpad_typing, validate_scratchpad_scroll, validate_scratchpad_frame_states

class ScratchpadEffectsTests(unittest.TestCase):
    def fixture(self):
        manifest = dict(body_hash='a'*64, body_bytes=12000,
                        prefixed_x_hash='b'*64, prefixed_x_bytes=12001)
        state = dict(ready=True, editor_focused=True, body_hash='a'*64, body_bytes=12000,
                     offset_x=0.0, offset_y=0.0, root_rect=[0,0,800,600],
                     editor_rect=[32,130,768,24000], scroll_rect=[32,130,768,480])
        edited = dict(state, body_hash='b'*64, body_bytes=12001)
        steps = [dict(kind='click',x=80.0,y=260.0), dict(kind='key',key='home'),
                 dict(kind='character',value='x'), dict(kind='key',key='backspace')]
        effects = [dict(id=0,before=state,after=state),dict(id=1,before=state,after=state),
                   dict(id=2,before=state,after=edited),dict(id=3,before=edited,after=state)]
        return dict(steps=steps), copy.deepcopy(effects), manifest

    def lines(self, effects):
        return ['qos-native-effect '+json.dumps(row) for row in effects]

    def test_exact_native_model_edits(self):
        scenario, effects, manifest = self.fixture()
        self.assertEqual(validate_scratchpad_typing(scenario,self.lines(effects),manifest),2)

    def test_wrong_or_unready_model_and_extra_scroll_fail(self):
        for key,value in [('body_hash','c'*64),('body_bytes',12002),('body_bytes',True),
                          ('ready',False),('editor_focused',False),('offset_y',2.0),('offset_x',float('nan')),
                          ('root_rect',[0,0,0,0]),('editor_rect',None)]:
            scenario,effects,manifest=self.fixture()
            effects[2]['after'][key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):
                validate_scratchpad_typing(scenario,self.lines(effects),manifest)

    def test_missing_reordered_and_noop_fail(self):
        scenario,effects,manifest=self.fixture()
        for changed in [effects[:-1],effects[::-1],effects+effects[:1]]:
            with self.assertRaises(ValueError):
                validate_scratchpad_typing(scenario,self.lines(changed),manifest)
        effects[2]['after']=effects[2]['before']
        with self.assertRaises(ValueError):
            validate_scratchpad_typing(scenario,self.lines(effects),manifest)

    def test_invalid_manifest_and_unsupported_steps_fail(self):
        scenario,effects,manifest=self.fixture()
        for key,value in [('body_hash','bad'),('prefixed_x_hash','a'*64),
                          ('prefixed_x_bytes',12003)]:
            bad=dict(manifest,**{key:value})
            with self.assertRaises(ValueError):
                validate_scratchpad_typing(scenario,self.lines(effects),bad)
        scenario['steps'][2]['value']='y'
        with self.assertRaises(ValueError):
            validate_scratchpad_typing(scenario,self.lines(effects),manifest)

    def test_scroll_requires_real_movement_and_unchanged_content(self):
        scenario,effects,manifest=self.fixture()
        a=effects[0]['before']
        b=dict(a,offset_y=144.0)
        steps=[dict(kind='wheel',dx=0.0,dy=-3.0),dict(kind='wheel',dx=0.0,dy=3.0)]
        rows=[dict(id=0,before=a,after=b),dict(id=1,before=b,after=a)]
        self.assertEqual(validate_scratchpad_scroll(dict(steps=steps),self.lines(rows),manifest),2)
        rows[0]['after']=a
        with self.assertRaises(ValueError):
            validate_scratchpad_scroll(dict(steps=steps),self.lines(rows),manifest)


    def test_post_draw_state_matches_last_input_in_each_batch(self):
        scenario,effects,_=self.fixture()
        frames=[dict(cpu_frame_id=10,input_ids=[0,1,2],state=effects[2]['after']),
                dict(cpu_frame_id=11,input_ids=[3],state=effects[3]['after'])]
        lines=lambda: self.lines(effects)+['qos-native-frame '+json.dumps(r) for r in frames]
        self.assertEqual(validate_scratchpad_frame_states(scenario,lines()),2)
        frames[0]['state']=effects[0]['after']
        with self.assertRaises(ValueError):
            validate_scratchpad_frame_states(scenario,lines())
        frames.pop()
        with self.assertRaises(ValueError):
            validate_scratchpad_frame_states(scenario,lines())
