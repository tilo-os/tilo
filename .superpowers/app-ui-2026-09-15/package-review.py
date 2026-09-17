from pathlib import Path
import subprocess,sys
plan,base,head=sys.argv[1:4]
repo=Path(__file__).resolve().parents[2]
w=repo/'.superpowers/sdd'/Path(plan).stem
w.mkdir(parents=True,exist_ok=True)
def git(*args):return subprocess.check_output(['git',*args],cwd=repo).decode()
r=w/f'review-{base[:9]}-{head[:9]}.md'
r.write_text(f'# Task review package\n\nPlan: {plan}\nBase: {base}\nHead: {head}\n\n## Commits\n\n```\n'+git('log','--oneline',f'{base}..{head}')+'```\n\n## Files\n\n```\n'+git('diff','--stat',base,head)+'```\n\n## Diff\n\n```diff\n'+git('diff','-U10',base,head)+'```\n')
print(r)
