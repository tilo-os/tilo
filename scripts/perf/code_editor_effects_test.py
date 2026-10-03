import copy
import hashlib
import json
import unittest

from code_editor_effects import (
    validate_code_editor_typing,
    validate_code_editor_scroll,
    validate_code_editor_frame_states,
)


class CodeEditorEffectsTests(unittest.TestCase):
    def fixture(self):
        source = b'pub fn main() {}\n' * 100
        digest = lambda data: hashlib.sha256(data).hexdigest()
        manifest = dict(schema_version=1, hash_algorithm='sha256', editor_id='editor_view',
                        lines=100, body_hash=digest(source), body_bytes=len(source),
                        prefixed_x_hash=digest(b'x' + source), prefixed_x_bytes=len(source)+1)
        state = dict(app='dev.tilo.code-editor', editor_id='editor_view', hash_algorithm='sha256',
                     buffer_id='file:fixture.rs', ready=True, body_hash=manifest['body_hash'],
                     body_bytes=len(source), editor_focused=True, caret_line=0, caret_column=0,
                     caret_count=1, selection_chars=0, offset_x=0.0, offset_y=0.0,
                     offset_x_unit='pixels', offset_y_unit='lines', root_rect=[0,0,800,600],
                     editor_rect=[200,40,800,560])
        edited = dict(state, body_hash=manifest['prefixed_x_hash'],
                      body_bytes=manifest['prefixed_x_bytes'], caret_column=1)
        steps = [dict(kind='key',key='home'),dict(kind='character',value='x'),
                 dict(kind='key',key='backspace')]
        effects = [dict(id=0,before=state,after=state),dict(id=1,before=state,after=edited),
                   dict(id=2,before=edited,after=state)]
        return dict(steps=steps), copy.deepcopy(effects), manifest

    @staticmethod
    def lines(effects, frames=()):
        return (['tilo-native-window '+json.dumps(dict(scale_factor=1.0, physical_width=800, physical_height=600))]
                + ['tilo-native-effect '+json.dumps(row) for row in effects]
                + ['tilo-native-frame '+json.dumps(row) for row in frames])

    def test_typing_accepts_only_exact_transitions_over_well_formed_evidence(self):
        with self.subTest(case='exact hash caret and focus transitions'):
            scenario,effects,manifest=self.fixture()
            self.assertEqual(validate_code_editor_typing(scenario,self.lines(effects),manifest),2)
        for key,value in [('body_hash','c'*64),('body_hash','bad'),('body_bytes',True),
                          ('body_bytes',12),('hash_algorithm','blake3'),('buffer_id',''),
                          ('editor_focused',False),('editor_focused',1),('caret_line',1),
                          ('caret_column',2),('caret_column',True),('caret_count',2),
                          ('selection_chars',1),('offset_y',1.0),('offset_x',float('nan')),
                          ('offset_y_unit','pixels'),('root_rect',[0,0,900,600]),
                          ('editor_rect',[200,40,800,550]),('ready',False),('app','other')]:
            scenario,effects,manifest=self.fixture()
            effects[1]['after'][key]=value
            with self.subTest(case='wrong or malformed state',key=key,value=value),self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,self.lines(effects),manifest)
        _,fixture,_=self.fixture()
        for key in fixture[0]['before']:
            scenario,effects,manifest=self.fixture()
            del effects[1]['after'][key]
            with self.subTest(case='missing field',key=key),self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,self.lines(effects),manifest)
        scenario,effects,manifest=self.fixture()
        for bad in [effects[::-1],effects[:-1],effects+effects[:1],None,[None]*3]:
            with self.subTest(case='reordered/missing/extra effects',bad=bad),self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,self.lines(bad or []),manifest)
        effects[2]['before']['caret_column']=0
        with self.subTest(case='discontinuous effects'),self.assertRaises(ValueError):
            validate_code_editor_typing(scenario,self.lines(effects),manifest)
        for change in [dict(kind='character',value='y'),dict(kind='key',key='delete'),
                       dict(kind='character',value='x',extra=True)]:
            scenario,effects,manifest=self.fixture();scenario['steps'][1]=change
            with self.subTest(case='unknown step',step=change),self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,self.lines(effects),manifest)
        for key,value in [('hash_algorithm','blake3'),('body_hash','bad'),
                          ('prefixed_x_bytes',3),('body_bytes',True),('lines',0)]:
            scenario,effects,manifest=self.fixture();manifest[key]=value
            with self.subTest(case='bad manifest',key=key),self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,self.lines(effects),manifest)
        scenario,effects,manifest=self.fixture();effects[1]['after']=effects[1]['before']
        with self.subTest(case='no-op edit'),self.assertRaises(ValueError):
            validate_code_editor_typing(scenario,self.lines(effects),manifest)
        scenario,_,manifest=self.fixture()
        for lines in [['tilo-native-effect {'],['tilo-native-effect null']*3]:
            with self.subTest(case='malformed json and records fail closed',lines=lines),self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,lines,manifest)

    def scroll_fixture(self):
        _,effects,manifest=self.fixture();a=effects[0]['before'];b=dict(a,offset_y=9.0)
        steps=[dict(kind='hover',x=300,y=100),dict(kind='wheel',dx=0.0,dy=-3.0),
               dict(kind='wheel',dx=0.0,dy=3.0)]
        effects=[dict(id=0,before=a,after=a),dict(id=1,before=a,after=b),dict(id=2,before=b,after=a)]
        return dict(steps=steps),copy.deepcopy(effects),manifest

    def test_scroll_requires_real_directed_motion_inside_editor_and_nothing_else(self):
        scenario,effects,manifest=self.scroll_fixture()
        self.assertEqual(validate_code_editor_scroll(scenario,self.lines(effects),manifest),2)
        for dy in [0,3,float('nan'),True]:
            bad=copy.deepcopy(scenario);bad['steps'][1]['dy']=dy
            with self.subTest(case='wheel direction',dy=dy),self.assertRaises(ValueError):
                validate_code_editor_scroll(bad,self.lines(effects),manifest)
        for x in [0,float('nan'),True]:
            bad=copy.deepcopy(scenario);bad['steps'][0]['x']=x
            with self.subTest(case='hover requires finite pointer inside editor',x=x),self.assertRaises(ValueError):
                validate_code_editor_scroll(bad,self.lines(effects),manifest)
        effects[1]['after']=effects[1]['before']
        with self.subTest(case='no motion'),self.assertRaises(ValueError):
            validate_code_editor_scroll(scenario,self.lines(effects),manifest)
        for key,value in [('caret_column',1),('editor_focused',False),('body_hash','c'*64),
                          ('buffer_id','different'),('root_rect',[0,0,801,600]),
                          ('editor_rect',[200,41,800,560]),('offset_x',1.0),
                          ('offset_y_unit','pixels')]:
            scenario,effects,manifest=self.scroll_fixture();effects[1]['after'][key]=value
            with self.subTest(case='scroll cannot change caret/focus/content/geometry',key=key),self.assertRaises(ValueError):
                validate_code_editor_scroll(scenario,self.lines(effects),manifest)

    def test_pointer_evidence_uses_one_valid_native_window_scale(self):
        window2x='tilo-native-window '+json.dumps(dict(scale_factor=2.0,physical_width=1600,physical_height=1200))
        with self.subTest(case='scroll hover is physical'):
            scenario,effects,manifest=self.scroll_fixture()
            scenario['steps'][0].update(x=1200,y=400)
            lines=self.lines(effects)
            lines[0]=window2x
            self.assertEqual(validate_code_editor_scroll(scenario,lines,manifest),2)
            # Physically inside the logical rectangle is insufficient: after /2,
            # x=150 lands outside the editor's logical left edge at200.
            scenario['steps'][0].update(x=300,y=100)
            with self.assertRaises(ValueError):
                validate_code_editor_scroll(scenario,lines,manifest)
        with self.subTest(case='typing click also uses physical coordinates'):
            scenario,effects,manifest=self.fixture()
            scenario['steps'][0]=dict(kind='click',x=1200,y=400)
            lines=self.lines(effects)
            lines[0]=window2x
            self.assertEqual(validate_code_editor_typing(scenario,lines,manifest),2)
            scenario['steps'][0]['x']=300
            with self.assertRaises(ValueError):
                validate_code_editor_typing(scenario,lines,manifest)
        scenario,effects,manifest=self.scroll_fixture()
        lines=self.lines(effects)
        for label,bad in [('missing window',lines[1:]),('duplicate window',lines[:1]+lines)]:
            with self.subTest(case=label),self.assertRaises(ValueError):
                validate_code_editor_scroll(scenario,bad,manifest)
        for value in [None,True,0,-1,float('nan'),float('inf'),'2']:
            bad=list(lines)
            bad[0]='tilo-native-window '+json.dumps(dict(scale_factor=value))
            with self.subTest(scale=value),self.assertRaises(ValueError):
                validate_code_editor_scroll(scenario,bad,manifest)
        bad=list(lines);bad[0]='tilo-native-window {}'
        with self.subTest(case='window without scale'),self.assertRaises(ValueError):
            validate_code_editor_scroll(scenario,bad,manifest)

    def frame_fixture(self):
        scenario,effects,manifest=self.fixture()
        frames=[dict(cpu_frame_id=10,input_ids=[0,1],state=copy.deepcopy(effects[1]['after'])),
                dict(cpu_frame_id=11,input_ids=[2],state=copy.deepcopy(effects[2]['after']))]
        return scenario,effects,frames

    def test_postdraw_matches_complete_last_input_state_through_a_valid_input_map(self):
        scenario,effects,frames=self.frame_fixture()
        self.assertEqual(validate_code_editor_frame_states(scenario,self.lines(effects,frames)),2)
        for key,value in [('caret_column',0),('body_hash','c'*64),('editor_focused',False),
                          ('buffer_id','different'),('editor_rect',[200,40,800,550]),
                          ('offset_y',9.0)]:
            bad=copy.deepcopy(frames);bad[0]['state'][key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):
                validate_code_editor_frame_states(scenario,self.lines(effects,bad))
        for label,bad in [('no frames',[]),('missing frame',frames[:-1]),
                          ('reordered frames',frames[::-1]),('extra frame',frames+frames[:1])]:
            with self.subTest(case=label),self.assertRaises(ValueError):
                validate_code_editor_frame_states(scenario,self.lines(effects,bad))
        for field,value in [('cpu_frame_id',True),('cpu_frame_id',-1),('input_ids',[]),
                            ('input_ids',[0,0]),('input_ids',[0,True]),('input_ids',[0,1,2,3])]:
            bad=copy.deepcopy(frames);bad[0][field]=value
            with self.subTest(field=field,value=value),self.assertRaises(ValueError):
                validate_code_editor_frame_states(scenario,self.lines(effects,bad))


if __name__=='__main__':
    unittest.main()
