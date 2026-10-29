 import json, sys
 sys.argv.append("/Users/xaver/dev/tilo-land-iframes/target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host")
 import page_actions as pa
 from native_regressions import Host
 pa.PageActions.setUpClass.__func__(pa.FramedActions)
 url=pa.FramedActions.url
 h=Host()
 h.send(op='visibility', visible=True)
 h.send(op='resize', width=1000, height=800, scale_factor=1.0)
 h.navigate(url+"/form")
 for i in (1,2,3):
     h.send(op='page', request_id=i, action='outline')
     r=h.until(lambda e: e['event']=='page_result' and e['request_id']==i)
     print(i, json.dumps(r.get('outline')))
 h.log.seek(0); print(h.log.read().decode()[-2500:]); h.p.kill()
