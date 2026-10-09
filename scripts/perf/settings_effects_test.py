import copy
import json
import unittest
from settings_effects import ALL_CATEGORIES, Z_CATEGORIES, validate_settings_typing, validate_settings_frame_states

class SettingsEffectsTests(unittest.TestCase):
    def fixture(self):
        a=dict(app='so.tilo.settings',search_id='settings.search',ready=True,
          local_search_text='',synchronized_search_query='',search_widget_text='',search_caret_byte=0,
          search_focused=True,search_rect=[20,10,780,45],root_rect=[0,0,800,600],selected_category='network',
          rendered_result_count=23,rendered_category_ids=list(ALL_CATEGORIES),narrow=False,sidebar_open=True,
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



class NavigationTests(unittest.TestCase):
    def test_mounted_navigation_is_not_a_latency_certificate(self):
        from settings_effects import validate_navigation_read
        value=dict(app='so.tilo.settings',selected_pane='mouse_keyboard',requested_pane='mouse_keyboard',
            path='settings://mouse_keyboard',pending=False,ready=True,content_visible=True,
            presentation_attribution_available=False)
        self.assertEqual(validate_navigation_read(value,'mouse_keyboard')['latency_available'],False)
        for key,bad in [('selected_pane','network'),('requested_pane','network'),('pending',True),
                ('ready',False),('content_visible',False),('presentation_attribution_available',True)]:
            with self.subTest(key=key),self.assertRaises(ValueError):
                validate_navigation_read(dict(value,**{key:bad}),'mouse_keyboard')

    def test_exact_selected_route_and_pinned_scope_precede_activation(self):
        from settings_effects import settings_search_navigation
        bound=('so.tilo.settings','owned-room','fresh-record')
        def row(resource,op):
            return dict(provider='app-actions',id=resource+'/'+op,application=bound[0],room=bound[1],
                record=bound[2],resource='opaque:' + resource, semantic_identity=dict(kind=('settings.section' if resource.startswith('settings.section:') else 'settings.navigation'),key=resource),operation=op,contract={'availability':{'state':'available'}})
        target=row('settings.section:mouse_keyboard','open-pane')
        inspector=row('settings.navigation','inspect-navigation')
        class Vm:
            def __init__(self):self.keys=[];self.selected=dict(provider='app-actions',id='unrelated')
            def control_json(self,*args):return {'query':bound[0],'actions':[target,inspector]}
            def key(self,key):
                self.keys.append(key)
                if key=='ArrowDown':self.selected={k:target[k] for k in ('provider','id')}
            def input(self,*args):pass
            def state(self):return {'chrome':{'palette':{'visible':True,'query':'keyboard','selected_action':self.selected}}}
        vm=Vm();scopes=[]
        def call(actual,args,name):
            self.assertEqual(actual,inspector)
            return dict(app=bound[0],selected_pane='mouse_keyboard',requested_pane='mouse_keyboard',
                path='settings://mouse_keyboard',pending=False,ready=True,content_visible=True,
                presentation_attribution_available=False)
        result=settings_search_navigation(vm,bound,lambda:scopes.append(True),call)
        self.assertEqual(vm.keys,['CTRL+K','ArrowDown','Enter'])
        self.assertGreater(len(scopes),3)
        self.assertTrue(result['navigation_verified'])
        vm=Vm()
        def refuse():raise ValueError('replaced owned native lifetime')
        with self.assertRaises(ValueError):settings_search_navigation(vm,bound,refuse,call)
        self.assertEqual(vm.keys,[])
        for key,bad in [('semantic_identity',None),('contract',{'availability':{'state':'unavailable'}})]:
            original=copy.deepcopy(target);target[key]=bad
            vm=Vm()
            with self.subTest(key=key),self.assertRaises(ValueError):
                settings_search_navigation(vm,bound,lambda:None,call)
            self.assertEqual(vm.keys,[])
            target.clear();target.update(original)
        class ReplacedDescriptorVm(Vm):
            reads=0
            def control_json(self,*args):
                self.reads+=1
                current=copy.deepcopy(target)
                if self.reads>=3:current['id']='replacement-action'
                return {'query':bound[0],'actions':[current,inspector]}
        vm=ReplacedDescriptorVm()
        with self.assertRaises(ValueError):settings_search_navigation(vm,bound,lambda:None,call)
        self.assertNotIn('Enter',vm.keys)
        vm=Vm();target['record']='old-record'
        with self.assertRaises(ValueError):settings_search_navigation(vm,bound,lambda:None,call)
        self.assertEqual(vm.keys,[])

class ActivationTests(unittest.TestCase):
    invocation = '12345678-1234-4567-8234-123456789abc'
    observer = 'abcdef12-1234-4567-8234-123456789abc'

    def fixture(self):
        def snapshot(pane):
            nav = dict(selected_pane=pane, requested_pane=pane, path='settings://' + pane,
                       ready=True, pending=False, content_visible=True)
            return dict(app='so.tilo.settings', navigation=nav, rendered_navigation=nav.copy())
        owner = dict(process_id=2, observer_session=self.observer)
        return [
            ('tilo-action-submission ', dict(invocation=self.invocation, application='so.tilo.settings',
                source='controlled_input', clock_id=1, input_time_ns=None,
                activation_dispatch_ns=100_000_000, submission_time_ns=101_000_000)),
            ('tilo-broker-input ', dict(owner, invocation=self.invocation,
                clock='clock_monotonic_seconds', handler_returned_ok=True,
                handler_dispatch_lo_s=.102, handler_dispatch_hi_s=.103,
                handler_return_lo_s=.104, handler_return_hi_s=.105,
                before=snapshot('network'), after=snapshot('mouse_keyboard'))),
            ('tilo-broker-frame ', dict(owner, invocations=[self.invocation], cpu_frame_id=17,
                state=snapshot('mouse_keyboard'))),
            ('tilo-display-frame ', dict(owner, backend='wayland', cpu_frame_id=17,
                clock_id=1, discarded=False, flags=7, presented_time_ns=120_000_000, sequence=11)),
        ]

    def report(self, rows):
        from settings_effects import summarize_settings_activation
        return summarize_settings_activation([p+json.dumps(r) for p,r in rows],
            self.invocation, 'mouse_keyboard', 2, self.observer)

    def test_control_submission_is_not_physical_ctrl_k_or_hover(self):
        report = self.report(self.fixture())
        self.assertEqual(report['submission_to_kms_ms'], 19)
        self.assertIsNone(report['physical_activation_to_kms_ms'])
        self.assertFalse(report['physical_activation_available'])
        self.assertFalse(report['palette_open_latency_available'])
        self.assertFalse(report['hover_latency_available'])
        rows=self.fixture();rows[0][1].update(source='physical_key',input_time_ns=90_000_000)
        self.assertEqual(self.report(rows)['physical_activation_to_kms_ms'],30)

    def test_physical_button_preserves_original_origin_and_same_draw_proof(self):
        rows = self.fixture()
        rows[0][1].update(source='physical_pointer_button', input_time_ns=90_000_000)
        result = self.report(rows)
        self.assertTrue(result['physical_activation_available'])
        self.assertEqual(result['source'], 'physical_pointer_button')
        self.assertEqual(result['physical_activation_to_kms_ms'], 30)
        self.assertEqual(result['cpu_frame_id'], 17)
        for key, value in (('input_time_ns', None), ('input_time_ns', 0),
                           ('input_time_ns', True), ('input_time_ns', 100_000_001),
                           ('activation_dispatch_ns', None), ('activation_dispatch_ns', 101_000_001),
                           ('clock_id', True), ('clock_id', 0)):
            changed = copy.deepcopy(rows)
            changed[0][1][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                self.report(changed)
        changed = copy.deepcopy(rows)
        changed[0][1]['source'] = 'controlled_input'
        with self.assertRaises(ValueError):
            self.report(changed)

    def test_all_exact_links_and_positive_feedback_are_required(self):
        for index in range(4):
            rows=self.fixture();del rows[index]
            with self.subTest(missing=index), self.assertRaises(ValueError):self.report(rows)
            rows=self.fixture();rows.append(copy.deepcopy(rows[index]))
            with self.subTest(duplicate=index), self.assertRaises(ValueError):self.report(rows)
        for index,key,value in [(1,'observer_session','another-worker'),(2,'process_id',3),
                (2,'invocations',[self.invocation,'other']),(3,'cpu_frame_id',18),
                (3,'flags',3),(3,'discarded',True),(3,'clock_id',0),(1,'handler_returned_ok',False),
                (0,'input_time_ns',99),(0,'submission_time_ns',110_000_000),
                (3,'presented_time_ns',100_000_000)]:
            rows=self.fixture();rows[index][1][key]=value
            with self.subTest(index=index,key=key),self.assertRaises(ValueError):self.report(rows)

    def test_actual_changed_drawn_pane_is_required(self):
        for target in ('before','after'):
            rows=self.fixture();rows[1][1][target]['navigation']['requested_pane']=('mouse_keyboard' if target=='before' else 'network')
            with self.subTest(target=target),self.assertRaises(ValueError):self.report(rows)
        for key,value in [('selected_pane','network'),('ready',False),('pending',True),('content_visible',False)]:
            rows=self.fixture();rows[2][1]['state']['rendered_navigation'][key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):self.report(rows)
        rows=self.fixture();rows[1][1]['before']=copy.deepcopy(rows[2][1]['state'])
        with self.assertRaises(ValueError):self.report(rows)

    def test_discarded_draw_requires_exact_negative_and_duplicate_kms_is_rejected(self):
        rows=self.fixture()
        earlier=copy.deepcopy(rows[2]);earlier[1]['cpu_frame_id']=16
        negative=copy.deepcopy(rows[3]);negative[1].update(cpu_frame_id=16,discarded=True)
        rows += [earlier,negative]
        self.assertEqual(self.report(rows)['cpu_frame_id'],17)
        rows[-1][1]['discarded']=False
        with self.assertRaises(ValueError):self.report(rows)

    def test_bounded_existing_flight_normalization(self):
        from settings_effects import activation_lines
        text='private unrelated log line\n'+'\n'.join('[flight] '+p+json.dumps(r) for p,r in self.fixture())
        self.assertEqual(self.report([(line.split(' ',1)[0]+' ',json.loads(line.split(' ',1)[1]))
                                     for line in activation_lines(text)])['submission_to_kms_ms'],19)
        for text in ['tilo-broker-frame []', 'tilo-broker-frame '+json.dumps({'value':'x'*8192}),
                     'x'*(8*1024*1024+1), 'tilo-broker-frame {"x":"tilo-display-frame "}']:
            with self.subTest(size=len(text)),self.assertRaises(ValueError):activation_lines(text)

if __name__=='__main__':unittest.main()
