from pathlib import Path
import concurrent.futures,hashlib,json,os,subprocess,struct
repo=Path(__file__).resolve().parents[2];w=Path(__file__).resolve().parent
specs=[
('calendar','calendar',['--screen','calendar'],{}),('calculator','live',[],{'QOS_CALC_EMPTY':'1'}),
('file-explorer','actual-size',[],{}),('pdf_viewer','sample',['--sample'],{}),('remote','sample',[],{'QOS_REMOTE_SAMPLE':'1'}),
('themes','controls',[],{'QOS_THEMES_SHOT':'scroll'}),('themes','live',[],{'QOS_THEMES_SHOT':'live'}),
('totp','add',['--screen','add'],{}),('weather','detail',['--scenario','detail'],{}),('vpn','detail',['--scenario','detail'],{}),
('store','detail',['--scenario','detail'],{}),('terminal','shell',['--scenario','shell'],{}),
('snippets','detail',['--scenario','detail'],{}),('typst_editor','errors',['--scenario','errors'],{}),
('sheets','editing',['--scenario','editing'],{}),('slides','inspector',['--scenario','inspector'],{}),
('video-player','library',['--scenario','library'],{}),('tube','search',['--route','search:rust'],{}),
('music','search',['--scenario','search-songs'],{}),('mail','compose',['--scenario','compose'],{})]

def one(spec):
 app,label,extra,custom=spec;out=w/'captures'/app;out.mkdir(exist_ok=True);rows=[]
 for variant,viewport in [('wide','1200x800'),('narrow','480x900')]:
  name=label+'-'+variant;png=out/(name+'.png');profile=out/(name+'-profile');profile.mkdir(exist_ok=True)
  command=[str(repo/'target/debug'/app),'--shots',str(png),'--size' if app=='file-explorer' else '--viewport',viewport,*extra]
  env=dict(os.environ,HOME=str(profile),XDG_DATA_HOME=str(profile/'data'),XDG_CONFIG_HOME=str(profile/'config'),QUICK_UI_VELLO_CPU='1',QOS_SHOTS_THEME='dark',**custom)
  try:
   with (out/(name+'.log')).open('w') as log:r=subprocess.run(command,cwd=repo,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=90)
   status='captured' if r.returncode==0 and png.exists() else 'failed'
  except subprocess.TimeoutExpired:status='timeout'
  rows.append(dict(app=app,name=name,command=command,environment=custom,status=status,path=str(png)))
  print(app,name,status,flush=True)
 return rows
rows=[]
with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
 for result in pool.map(one,specs):rows+=result
(w/'supplement-results.json').write_text(json.dumps(rows,indent=2))
