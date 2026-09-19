import copy
import json
import unittest
from file_explorer_effects import validate_file_explorer_scroll, validate_file_explorer_frame_states


class FileExplorerEffectsTests(unittest.TestCase):
    def fixture(self):
        manifest = dict(schema=1,hash_algorithm='sha256',entry_count=1200,image_count=299,directory_count=4,order_hash='a'*64)
        state = dict(app='dev.qos.file-explorer',view_id='file_view',directory='/tmp/fixture',entry_count=1200,
            order_hash='a'*64,hash_algorithm='sha256',ready=True,is_loading=False,has_error=False,
            thumbnail_pending=10,selected_indices=[],selected_names=[],primary_index=None,
            offset_x=0.0,offset_y=0.0,offset_units='pixels',root_rect=[0,0,800,600],
            viewport_rect=[200,60,800,560],content_rect=[200,60,800,5060],
            view_mode='Grid',sort_by='Name',sort_order='Ascending')
        moved=dict(state,offset_y=90.0,content_rect=[200,-30,800,4970],thumbnail_pending=9)
        effects=[dict(id=0,before=state,after=state),dict(id=1,before=state,after=moved),dict(id=2,before=moved,after=state)]
        scenario=dict(steps=[dict(kind='hover',x=600,y=200),dict(kind='wheel',dx=0,dy=-3),dict(kind='wheel',dx=0,dy=3)])
        frames=[dict(cpu_frame_id=i+10,input_ids=[i],state=copy.deepcopy(e['after'])) for i,e in enumerate(effects)]
        return scenario,copy.deepcopy(effects),manifest,frames

    def lines(self,effects,frames=(),scale=2):
        return ['qos-native-window '+json.dumps(dict(scale_factor=scale))]+['qos-native-effect '+json.dumps(e) for e in effects]+['qos-native-frame '+json.dumps(f) for f in frames]

    def test_real_movement_and_postdraw_with_async_thumbnail_progress(self):
        s,e,m,f=self.fixture()
        f[1]['state']['thumbnail_pending']=0
        self.assertEqual(validate_file_explorer_scroll(s,self.lines(e),m),2)
        self.assertEqual(validate_file_explorer_frame_states(s,self.lines(e,f)),3)

    def test_bad_manifest_rejected(self):
        for k,v in [('schema',True),('entry_count',1),('order_hash','b'*64),('hash_algorithm','none'),('image_count',1201),('directory_count',-1)]:
            s,e,m,f=self.fixture();m[k]=v
            with self.subTest(k=k),self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(e),m)

    def test_invalid_changed_or_noop_states_rejected(self):
        for k,v in [('app','other'),('offset_y',0),('offset_y',99999),('offset_y',float('nan')),('offset_x',True),('entry_count',1199),('directory','/elsewhere'),('order_hash','b'*64),('view_mode','List'),('sort_order','Descending'),('selected_indices',[0]),('root_rect',[0,0,0,0]),('viewport_rect',None),('content_rect',[0,0,1,-1]),('ready',False)]:
            s,e,m,f=self.fixture();e[1]['after'][k]=v
            with self.subTest(k=k),self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(e),m)

    def test_pointer_requires_physical_scale_and_viewport_hit(self):
        s,e,m,f=self.fixture()
        for scale in [None,0,True,float('nan')]:
            with self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(e,scale=scale),m)
        for lines in [self.lines(e)[1:], self.lines(e)[:1]+self.lines(e)]:
            with self.assertRaises(ValueError):validate_file_explorer_scroll(s,lines,m)
        s['steps'][0]['x']=200
        with self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(e),m)

    def test_frame_maps_and_surviving_state_strict(self):
        s,e,m,f=self.fixture()
        for frames in [[],f[:-1],f[::-1],f+f[:1]]:
            with self.assertRaises(ValueError):validate_file_explorer_frame_states(s,self.lines(e,frames))
        for k,v in [('offset_y',0),('entry_count',1),('input_ids',[1,1]),('cpu_frame_id',True)]:
            changed=copy.deepcopy(f)
            if k in ('input_ids','cpu_frame_id'):changed[1][k]=v
            else:changed[1]['state'][k]=v
            with self.assertRaises(ValueError):validate_file_explorer_frame_states(s,self.lines(e,changed))
        merged=[dict(cpu_frame_id=10,input_ids=[0,1],state=e[1]['after']),f[2]]
        self.assertEqual(validate_file_explorer_frame_states(s,self.lines(e,merged)),2)

    def test_stale_immediate_geometry_is_allowed_but_postdraw_must_translate(self):
        s,e,m,f=self.fixture()
        e[1]['after']['content_rect']=e[0]['after']['content_rect']
        self.assertEqual(validate_file_explorer_scroll(s,self.lines(e),m),2)
        self.assertEqual(validate_file_explorer_frame_states(s,self.lines(e,f)),3)
        f[1]['state']['content_rect']=e[0]['after']['content_rect']
        with self.assertRaises(ValueError):validate_file_explorer_frame_states(s,self.lines(e,f))

    def test_async_pending_is_allowed_but_malformed_counts_and_missing_fields_fail(self):
        s,e,m,f=self.fixture()
        e[1]['before']['thumbnail_pending']=None
        e[1]['after']['thumbnail_pending']=0
        self.assertEqual(validate_file_explorer_scroll(s,self.lines(e),m),2)
        for key in list(e[1]['after']):
            broken=copy.deepcopy(e);del broken[1]['after'][key]
            with self.subTest(key=key),self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(broken),m)
        for value in [True,-1,1.5]:
            broken=copy.deepcopy(e);broken[1]['after']['thumbnail_pending']=value
            with self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(broken),m)

    def test_missing_duplicate_malformed_effects_rejected(self):
        s,e,m,f=self.fixture()
        for effects in [e[:-1],e+e[:1],e[::-1],[None]*3]:
            with self.assertRaises(ValueError):validate_file_explorer_scroll(s,self.lines(effects),m)

if __name__=='__main__':unittest.main()
