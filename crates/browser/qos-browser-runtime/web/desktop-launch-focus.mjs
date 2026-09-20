// Only a current, mount-owned user launch may select a committed native tab.
// No recovered action, remote topology, label match, or timer creates this token.
export function createDesktopLaunchFocus({snapshot,capture=()=>undefined,select,now=Date.now}) {
 const tokens=new Map(),seen=new Set();let closed=false;
 const same=(a,b)=>JSON.stringify(a)===JSON.stringify(b);
 function pending(value){
  if(closed||seen.has(value.invocation))return;
  seen.add(value.invocation);
  if(seen.size>256)seen.delete(seen.values().next().value);
  let command;try{command=JSON.parse(value.command);}catch{return;}
  if(!command.command?.OpenTab||typeof command.source?.room!=='string')return;
  // The newest launch supersedes earlier launch-focus intent, not their commits.
  tokens.clear();tokens.set(value.invocation,{generation:snapshot().generation,room:command.source.room,guard:capture(),deadline:now()+30000});
 }
 function refresh(){
  if(closed)return;
  for(const [invocation,token] of tokens){
   const state=snapshot();
   if(now()>=token.deadline||state.generation!==token.generation||!same(token.guard,capture())){tokens.delete(invocation);continue;}
   const event=token.event;if(!event)continue;
   const entries=state.entries?.filter(({route})=>state.workspaces?.some(workspace=>workspace.id===route.workspace&&workspace.room===event.room)&&['strip','group','tab'].every(key=>typeof event.opened?.[key]==='string'&&route[key]===event.opened[key]));
   if(entries?.length!==1)continue;
   const entry=entries[0];
   tokens.delete(invocation);
   // Consume first: dispatch synchronously refreshes the mount's projection.
   Promise.resolve(select(entry.route)).catch(()=>{});
  }
 }
 return {pending,refresh,committed(event){const token=tokens.get(event?.invocation);if(!token||token.room!==event.room||!event.opened)return;token.event=event;refresh();},
  updated(row){if(row.terminal&&row.state!=='succeeded')tokens.delete(row.invocation);},
  cancel(){tokens.clear();},close(){closed=true;tokens.clear();seen.clear();}};
}
