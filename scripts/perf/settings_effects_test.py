import copy
import json
import unittest
from settings_effects import ALL_CATEGORIES, Z_CATEGORIES, validate_settings_typing, validate_settings_frame_states

class SettingsEffectsTests(unittest.TestCase):
    def fixture(self):
        a=dict(app='so.tilo.settings',search_id='settings.search',ready=True,
          local_search_text='',synchronized_search_query='',search_widget_text='',search_caret_byte=0,
          search_focused=True,search_rect=[20,10,780,45],root_rect=[0,0,800,600],selected_category='network',
          rendered_result_count=len(ALL_CATEGORIES),rendered_category_ids=list(ALL_CATEGORIES),narrow=False,sidebar_open=True,
          document_root='Root',document_actor='ab12',document_witness_kind='effective_heads_on_disposable_clone',
          effective_document_heads=['a'*64],original_pending_ops=3)
        b=dict(a,local_search_text='z',search_widget_text='z',search_caret_byte=1)
        c=dict(b,synchronized_search_query='z',rendered_result_count=len(Z_CATEGORIES),rendered_category_ids=list(Z_CATEGORIES))
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
        category_source=source.split('impl Category {',1)[1]
        slugs=dict(re.findall(r'Category::(\w+)\s*=>\s*"([^"\n]*)"',category_source.split('pub fn slug(',1)[1].split('#[cfg(test)]',1)[0]))
        order=re.findall(r'Category::(\w+)',category_source.split('pub const ALL:',1)[1].split('];',1)[0])
        store=Path(__file__).resolve().parents[2]/'crates/system/settings-store'
        index=(store/'src/index.rs').read_text()
        pages=re.findall(r'Page\s*\{\s*slug:\s*"([^"]+)"\s*,?\s*\}',index.split('pub const PAGES:',1)[1].split('];',1)[0])
        settings=re.findall(r's\(\s*"([^"]+)",\s*"([^"]+)",\s*"([^"]+)"\s*,?\s*\)',index.split('pub const SETTINGS:',1)[1].split('];',1)[0])
        locale_paths=re.findall(r'include_str!\("([^"\n]+)"\)',index.split('pub const CATALOGUES:',1)[1].split('];',1)[0])
        self.assertTrue(pages)
        self.assertTrue(settings)
        self.assertTrue(locale_paths)
        self.assertEqual(len(pages),len(set(pages)))
        self.assertEqual(pages,[slugs[name] for name in order])
        catalogues=[dict(re.findall(r'^([^#\s][^=]*?) = (.*)$',(store/'src'/path).read_text(),re.M)) for path in locale_paths]
        def key(value):return value.replace('_','-').replace('.','-')
        def word_start(text):
            text=text.lower()
            return any(char=='z' and (at==0 or not text[at-1].isalnum()) for at,char in enumerate(text))
        expected=[]
        for page in pages:
            title='settings-index-page-'+key(page)
            keys=[title,title+'-words']
            keys += ['settings-index-setting-'+key(identity)+suffix
                     for identity,owner,_ in settings if owner==page for suffix in ('','-words')]
            # NavList matches labels by substring, but search terms only at
            # word starts. Terms include every language; subtitles use the
            # default English locale of this benchmark's isolated launch.
            terms=[catalogue[k] for catalogue in catalogues for k in keys]
            terms.append(catalogues[0][title+'-subtitle'])
            if 'z' in catalogues[0][title].lower() or any(word_start(term) for term in terms):
                expected.append(page)
        self.assertEqual(list(ALL_CATEGORIES),pages)
        self.assertEqual(list(Z_CATEGORIES),expected)

    def test_observations_require_current_catalogue_identity_and_exact_results(self):
        s,e,f=self.fixture()
        self.assertEqual(validate_settings_typing(s,self.lines(e)),2)
        self.assertEqual(validate_settings_frame_states(s,self.lines(e,f)),3)
        for ids in [list(Z_CATEGORIES)[::-1],list(Z_CATEGORIES)[:-1],
                    list(Z_CATEGORIES)+['keyboard'],list(Z_CATEGORIES)+[Z_CATEGORIES[0]],
                    ['keyboard','appearance','accessibility','time_date']]:
            with self.subTest(ids=ids):
                broken=copy.deepcopy(e)
                broken[2]['before'].update(rendered_category_ids=ids,rendered_result_count=len(ids))
                with self.assertRaises(ValueError):validate_settings_typing(s,self.lines(broken))
                frames=copy.deepcopy(f)
                frames[1]['state'].update(rendered_category_ids=ids,rendered_result_count=len(ids))
                with self.assertRaises(ValueError):validate_settings_frame_states(s,self.lines(e,frames))
        for app in ['dev.tilo.settings','other']:
            broken=copy.deepcopy(e);broken[1]['after']['app']=app
            with self.subTest(app=app),self.assertRaises(ValueError):
                validate_settings_typing(s,self.lines(broken))

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
