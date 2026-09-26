import copy
import json
import unittest
from settings_effects import ALL_CATEGORIES, Z_CATEGORIES, validate_settings_typing, validate_settings_frame_states

class SettingsEffectsTests(unittest.TestCase):
    def fixture(self):
        a=dict(app='dev.tilo.settings',search_id='settings.search',ready=True,
          local_search_text='',synchronized_search_query='',search_widget_text='',search_caret_byte=0,
          search_focused=True,search_rect=[20,10,780,45],root_rect=[0,0,800,600],selected_category='network',
          rendered_result_count=23,rendered_category_ids=list(ALL_CATEGORIES),narrow=False,sidebar_open=True,
          document_root='Root',document_actor='ab12',document_witness_kind='effective_heads_on_disposable_clone',
          effective_document_heads=['a'*64],original_pending_ops=3)
        b=dict(a,local_search_text='z',search_widget_text='z',search_caret_byte=1)
        c=dict(b,synchronized_search_query='z',rendered_result_count=3,rendered_category_ids=list(Z_CATEGORIES))
        d=dict(c,local_search_text='',search_widget_text='',search_caret_byte=0)
        scenario=dict(warmup_steps=1,steps=[dict(kind='click',x=100,y=50),dict(kind='character',value='z'),dict(kind='key',key='backspace')])
        effects=[dict(id=0,before=a,after=a),dict(id=1,before=a,after=b),dict(id=2,before=c,after=d)]
        frames=[dict(cpu_frame_id=10,input_ids=[0],state=a),dict(cpu_frame_id=11,input_ids=[1],state=c),dict(cpu_frame_id=12,input_ids=[2],state=a)]
        return scenario,copy.deepcopy(effects),copy.deepcopy(frames)
    def lines(self,e,f=(),scale=2):
        return ['tilo-native-window '+json.dumps(dict(scale_factor=scale))]+['tilo-native-effect '+json.dumps(v) for v in e]+['tilo-native-frame '+json.dumps(v) for v in f]
    def test_fixture_contract_matches_current_production_catalogue(self):
        from pathlib import Path
        import re
        source=(Path(__file__).resolve().parents[2]/'apps/settings/src/categories.rs').read_text()
        def method(name,next_name):
            body=source.split('pub fn '+name+'(',1)[1].split('pub fn '+next_name+'(',1)[0]
            return dict(re.findall(r'Category::(\w+)\s*=>\s*(?:\{\s*)?"([^"\n]*)"',body))
        # Section::label appears first; restrict to the Category implementation.
        category_source=source.split('impl Category {',1)[1]
        label_body=category_source.split('pub fn label(',1)[1].split('pub fn subtitle(',1)[0]
        labels=dict(re.findall(r'Category::(\w+)\s*=>\s*"([^"\n]*)"',label_body))
        subtitles=method('subtitle','icon')
        slugs=dict(re.findall(r'Category::(\w+)\s*=>\s*"([^"\n]*)"',source.split('pub fn slug(',1)[1].split('#[cfg(test)]',1)[0]))
        order=re.findall(r'Category::(\w+)',category_source.split('pub const ALL:',1)[1].split('];',1)[0])
        constants={name:re.findall(r'"([^"\n]*)"',body) for name,body in re.findall(r'const (\w+_KEYWORDS):[^=]*=\s*&\[(.*?)\];',source,re.S)}
        keyword_names=dict(re.findall(r'Category::(\w+)\s*=>\s*(\w+_KEYWORDS)',source))
        expected=[slugs[name] for name in order if 'z' in labels[name].lower() or 'z' in subtitles[name].lower()
                  or any('z' in key or key in 'z' for key in constants[keyword_names[name]])]
        self.assertEqual(list(ALL_CATEGORIES),[slugs[name] for name in order])
        self.assertEqual(list(Z_CATEGORIES),expected)

    def test_exact_and_coalesced_transitions_validate_through_postdraw(self):
        with self.subTest(phase='exact transition and postdraw'):
            s,e,f=self.fixture()
            self.assertEqual(validate_settings_typing(s,self.lines(e)),2)
            self.assertEqual(validate_settings_frame_states(s,self.lines(e,f)),3)
        with self.subTest(phase='commit between input and frame does not change effective heads'):
            s,e,f=self.fixture();f[1]['state']['original_pending_ops']=0
            self.assertEqual(validate_settings_frame_states(s,self.lines(e,f)),3)
        with self.subTest(phase='coalesced inputs still require final synchronized results'):
            s,e,f=self.fixture();e[2]['before']=copy.deepcopy(e[1]['after']);e[2]['after']=copy.deepcopy(e[0]['after'])
            frames=[f[0],dict(cpu_frame_id=11,input_ids=[1,2],state=f[2]['state'])]
            self.assertEqual(validate_settings_typing(s,self.lines(e)),2)
            self.assertEqual(validate_settings_frame_states(s,self.lines(e,frames)),2)

    def test_wrong_incomplete_or_misplaced_typing_evidence_fails(self):
        with self.subTest(phase='malformed and wrong state'):
            for k,v in [('ready',False),('app','other'),('search_focused',False),('search_caret_byte',0),('local_search_text','x'),('search_widget_text','x'),('effective_document_heads',['b'*64]),('document_actor','bc'),('original_pending_ops',4),('selected_category','about'),('narrow',True),('root_rect',[0,0,0,1])]:
                s,e,f=self.fixture();e[1]['after'][k]=v
                with self.subTest(key=k),self.assertRaises(ValueError):validate_settings_typing(s,self.lines(e))
        with self.subTest(phase='exact results not just narrowing'):
            for ids in [['appearance'],['appearance','time_date','accessibility'],['appearance','accessibility','about']]:
                s,e,f=self.fixture();e[2]['before']['rendered_category_ids']=ids;e[2]['before']['rendered_result_count']=len(ids)
                with self.assertRaises(ValueError):validate_settings_typing(s,self.lines(e))
        with self.subTest(phase='retina scale and setup boundary'):
            s,e,f=self.fixture()
            for scale in [0,True,None,float('nan')]:
                with self.assertRaises(ValueError):validate_settings_typing(s,self.lines(e,scale=scale))
            s['steps'][0]['y']=150
            with self.assertRaises(ValueError):validate_settings_typing(s,self.lines(e))
            s,e,f=self.fixture();s['warmup_steps']=0
            with self.assertRaises(ValueError):validate_settings_typing(s,self.lines(e))
        with self.subTest(phase='missing fields and effect identity'):
            s,e,f=self.fixture()
            for key in e[1]['after']:
                broken=copy.deepcopy(e);del broken[1]['after'][key]
                with self.subTest(key=key),self.assertRaises(ValueError):validate_settings_typing(s,self.lines(broken))
            for broken in [e[:-1],e+e[:1],e[::-1]]:
                with self.assertRaises(ValueError):validate_settings_typing(s,self.lines(broken))
        with self.subTest(phase='missing, duplicate and wrong frame states'):
            s,e,f=self.fixture()
            for frames in [[],f[:-1],f[::-1],f+f[:1]]:
                with self.assertRaises(ValueError):validate_settings_frame_states(s,self.lines(e,frames))
            for k,v in [('synchronized_search_query',''),('search_caret_byte',0),('effective_document_heads',['b'*64]),('original_pending_ops',4)]:
                broken=copy.deepcopy(f);broken[1]['state'][k]=v
                with self.assertRaises(ValueError):validate_settings_frame_states(s,self.lines(e,broken))


if __name__=='__main__':unittest.main()
