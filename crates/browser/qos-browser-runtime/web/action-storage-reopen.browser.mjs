// Browser-only regression. Supply an isolated enrollment host/profile session.
// Real IndexedDB, production PeerSession, compiled WASM and actual desktop mounts.
export async function runActionStorageReopen({bundle,enrollmentBaseUrl,target,log,runId}) {
  const [{createConnectPeer},{acquireStorage,createIdentityStore},{createIdentityCustodian},{mountDesktop},wasm]=await Promise.all([
    import(`${bundle}/peer-session.mjs`), import(`${bundle}/storage.mjs`),
    import(`${bundle}/identity-custody.mjs`), import(`${bundle}/host.mjs`),
    import(`${bundle}/pkg/qos_browser_runtime.js`),
  ]);
  const assert=(condition,message)=>{if(!condition)throw Error(message);};
  const failures=[],requests=[];
  let loseResponse=true,peer,mounted,mountInstance=0;
  const invocation=crypto.randomUUID();
  const runtime={...wasm,
    peer_action_request(id,raw){requests.push(JSON.parse(raw));return wasm.peer_action_request(id,raw);},
    peer_action_response(id,request){
      const raw=wasm.peer_action_response(id,request);
      if(raw&&loseResponse&&JSON.parse(raw).type==='observation')throw Error('regression: response lost after actual broker observation');
      return raw;
    },
  };
  const session={
    getItem:key=>sessionStorage.getItem(`${runId}:${key}`),
    setItem:(key,value)=>sessionStorage.setItem(`${runId}:${key}`,value),
    removeItem:key=>sessionStorage.removeItem(`${runId}:${key}`),
  };
  const connect=createConnectPeer({init:wasm.default,runtime,mountDesktop,acquireStorage,
    acquireIdentity:createIdentityCustodian({runtime,locks:navigator.locks,store:createIdentityStore()}),session});
  async function ready(current){
    const end=Date.now()+30000;
    while(current.getSnapshot().phase!=='ready'){
      if(Date.now()>end)throw Error(`peer not ready: ${current.getSnapshot().error??current.getSnapshot().phase}`);
      await new Promise(resolve=>setTimeout(resolve,25));
    }
  }
  function surface(){const node=document.createElement('div');node.style.cssText='width:900px;height:600px';target.replaceChildren(node);node.scrollIntoView({block:'start'});return node;}
  async function mount(current){
    const abort=new AbortController();
    const timer=setTimeout(()=>abort.abort(Error('regression mount deadline exceeded')),20000);
    try{return await current.mountDesktop(surface(),{instance:++mountInstance,signal:abort.signal});}
    finally{clearTimeout(timer);}
  }
  try {
    peer=await connect({enrollmentBaseUrl});await ready(peer);
    const identity=peer.getSnapshot().deviceId;
    mounted=await mount(peer);
    log('baseline actual WASM desktop mounted');
    const catalogue=await peer.actionsV1.catalogue();
    const route=catalogue.catalogue.surfaces.find(row=>row.app_id==='so.tilo.word-processor'&&row.actions.some(action=>action.action==='insert-table'));
    assert(route,'real Writer action catalogue missing');
    const descriptor=route.actions.find(action=>action.action==='insert-table');
    await peer.actionsV1.invoke({invocation,app_peer:String(route.peer_id),app_id:route.app_id,record_id:route.record_id,
      resource:descriptor.resource,action:descriptor.action,revision:null}).then(()=>{throw Error('lost response fixture did not run');},error=>assert(String(error).includes('response lost'),'unexpected invocation failure'));
    const before=peer.actionsV1.getSnapshot().entries.find(row=>row.invocation===invocation);
    assert(before&&!before.terminal,'original invocation must remain uncertain');
    const durable=await peer.durableState();
    log(`private invocation committed; ${durable.length} real IndexedDB rows`);
    await mounted.cleanup();mounted=undefined;
    try {
      const next=await mount(peer);await next.cleanup();
      log('subsequent actual WASM desktop hydration passed');
    } catch(error) {failures.push(`subsequent desktop hydration: ${String(error)}`);log(failures.at(-1));}
    await peer.dispose();peer=undefined;mounted=undefined;
    loseResponse=false;
    try {
      peer=await connect({enrollmentBaseUrl});await ready(peer);
      assert(peer.getSnapshot().deviceId===identity,'reopen changed certified device');
      const retained=peer.actionsV1.getSnapshot().entries.find(row=>row.invocation===invocation);
      assert(retained,'reopen lost original invocation');
      assert(JSON.stringify(retained.binding)===JSON.stringify(before.binding),'reopen changed original binding');
      assert(JSON.stringify(retained.intent)===JSON.stringify(before.intent),'reopen changed immutable intent');
      let observed;
      const deadline=Date.now()+15000;
      do {observed=await peer.actionsV1.status(invocation);if(observed.terminal)break;await new Promise(resolve=>setTimeout(resolve,50));}while(Date.now()<deadline);
      assert(observed?.state==='succeeded'&&observed.terminal,'original real broker effect did not complete');
      const reopened=await peer.durableState();
      assert(reopened.every(row=>typeof row.key==='string'),'persisted storage key is not a string');
      const privateRow=reopened.find(row=>JSON.parse(row.key).type==='privateActions');
      assert(privateRow,'serialized private key not found after reopening');
      assert(JSON.parse(new TextDecoder().decode(privateRow.bytes)).entries.find(row=>row.invocation===invocation)?.terminal,'terminal status was not durably saved');
      const next=await mount(peer);await next.cleanup();
      assert(requests.filter(row=>row.type==='invoke_bound'&&row.invocation===invocation).length===1,'recovery invoked the effect again');
      assert(requests.some(row=>row.type==='status'&&row.invocation===invocation),'recovery did not query original invocation');
      log('reopen preserved certified identity and full invocation; Status-only recovery durably completed; desktop mounted');
    } catch(error) {failures.push(`peer reopen/status recovery: ${String(error)}`);log(failures.at(-1));}
    if(failures.length)throw Error(failures.join('\n'));
    return {invocation,invokeCount:requests.filter(row=>row.type==='invoke_bound').length,statusCount:requests.filter(row=>row.type==='status').length};
  } finally {
    await peer?.dispose();
    await mounted?.cleanup();
  }
}
