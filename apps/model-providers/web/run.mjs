// Production WASM UI + FetchTransport proof. Build with the model-providers-web
// t check, or pass an existing single-app production bundle as the argument.
import assert from 'node:assert/strict';
import {createServer} from 'node:https';
import {createReadStream, existsSync, readFileSync, statSync} from 'node:fs';
import {ephemeralTls} from './https-fixture.mjs';
import {resolve, join, extname, sep} from 'node:path';
import {launch, sleep} from '../../../tools/web/chrome.mjs';

const bundle = resolve(process.argv[2] ?? 'target/tilo-app/web/model-providers/bundle');
assert(existsSync(join(bundle, 'pkg/tilo_browser_runtime_bg.wasm')), 'build the production app bundle first');
const metadata = JSON.parse(readFileSync(join(bundle, 'build.json')));
assert.equal(metadata.app, 'so.tilo.model-providers');
let browser, server;
const secret = 'private-browser-provider-key-92731';
const token = 'explicit-provider-management-session';
let config = {profiles: []}, revision = 0, rejectWrite = false, rejectKey = false;
let releaseFailure;
const credentials = new Map(), calls = [];
const snapshot = () => ({config: structuredClone(config), revision: String(revision)});
const mime = {'.js':'text/javascript', '.mjs':'text/javascript', '.wasm':'application/wasm', '.json':'application/json', '.html':'text/html', '.ttf':'font/ttf', '.otf':'font/otf'};
try {
  server = createServer(ephemeralTls(), async (req,res) => {
    try {
      if (req.url === '/model/rpc') {
        assert.equal(req.method, 'POST');
        assert.equal(req.headers.authorization, `Bearer ${token}`);
        let body = '';
        for await (const chunk of req) body += chunk;
        const request = JSON.parse(body);
        assert.equal(request.version, 3);
        calls.push(request);
        let output;
        switch(request.verb) {
          case 'get_provider_config': output = snapshot(); break;
          case 'set_provider_config':
            assert.equal(request.expected_revision, String(revision));
            if (rejectWrite) {
              rejectWrite = false;
              await new Promise(resolve => {releaseFailure = resolve;});
              res.writeHead(503).end(); return;
            }
            config = structuredClone(request.config); revision++; output = snapshot(); break;
          case 'credential_status': output = credentials.has(request.profile_id) ? 'configured' : 'missing'; break;
          case 'set_api_key':
            if (rejectKey) {
              rejectKey = false;
              await new Promise(resolve => {releaseFailure = resolve;});
              res.writeHead(503).end(); return;
            }
            credentials.set(request.profile_id, request.api_key); break;
          case 'remove_api_key': credentials.delete(request.profile_id); break;
          case 'remove_legacy_api_key': break;
          default: throw Error(`unexpected administration request: ${request.verb}`);
        }
        res.writeHead(200, {'content-type':'application/x-ndjson'}).end(JSON.stringify({ok:true, ...(output === undefined ? {} : {output:JSON.stringify(output)})})+'\n');
        return;
      }
      if (req.url === '/proof.html') {
        res.writeHead(200, {'content-type':'text/html'}).end('<!doctype html><html><body style="margin:0"><main id="app" style="width:1100px;height:1800px"></main></body></html>'); return;
      }
      const path = resolve(bundle, '.' + new URL(req.url, 'https://localhost').pathname);
      if (!path.startsWith(bundle + sep) || !existsSync(path) || !statSync(path).isFile()) {res.writeHead(404).end();return;}
      res.writeHead(200, {'content-type':mime[extname(path)] ?? 'application/octet-stream'});
      createReadStream(path).pipe(res);
    } catch(error) {console.error(error);res.writeHead(500).end();}
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  // Headless Linux workers use the same explicit WebGPU backend as the
  // desktop browser journeys, rather than relying on automatic driver selection.
  const gpuArgs = process.platform === 'linux'
    ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']
    : [];
  browser = await launch({args:['--ignore-certificate-errors', '--window-size=1200,2000', ...gpuArgs, ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
  const tab = await browser.tab();
  await tab.navigate(`${origin}/proof.html`);
  await tab.waitFor('!!document.getElementById("app")', {timeout:15000});
  await tab.evaluate(`(async()=>{
    const {mountDesktop}=await import('/host.mjs');
    window.semanticDeliveries=0;
    window.publicReports=[];
    window.mount=await mountDesktop(document.getElementById('app'), {
      app:'so.tilo.model-providers', profile:'provider-browser-proof', device:'browser',
      onStatus:state=>{if(state.semantics)semanticDeliveries++;publicReports.push(JSON.stringify(state));},
      providerManagement:{endpoint:location.origin+'/model/rpc',sessionToken:${JSON.stringify(token)}}
    });
    window.findControl=(id)=>{
      const walk=n=>n&&(n.test_id===id?n:(n.children??[]).map(walk).find(Boolean));
      return walk(mount.state()?.semantics?.root);
    };
    window.control=(id,action,value='')=>{
      const n=findControl(id); if(!n)throw Error('missing production control '+id);
      // The editor's value operation belongs to the widget itself. A named
      // semantic target addresses a synthetic child (such as its accessory).
      const target=action==='value'&&['TextInput','MultilineTextInput'].includes(n.semantics?.role)
        ? undefined : n.semantics?.action_target;
      if(mount.semantic(n.path,target,action,value,0,0)!==true)throw Error('rejected '+id+' '+action);
    };
    window.privateSurface=()=>JSON.stringify({reports:publicReports,state:mount.state(),actions:mount.actions(),publications:mount.publications(),projection:mount.semantics.outerHTML,editorValues:[...mount.semantics.querySelectorAll('input,textarea')].map(e=>e.value)});
    return true;
  })()`);
  const wait = expression => tab.waitFor(expression, {timeout:30000});
  const backend = async predicate => {
    const deadline = Date.now() + 30000;
    while (!predicate()) {
      assert(Date.now() < deadline, 'production UI did not complete the expected HTTPS operation; ' + JSON.stringify({verbs:calls.map(c=>c.verb),profiles:config.profiles.map(p=>({label:p.label,default_model:p.default_model,endpoint:p.endpoint})),runtime:await tab.evaluate('JSON.stringify({phase:mount.state()?.phase,error:mount.state()?.error})'),browserErrors:tab.log}));
      await sleep(25);
    }
  };
  const settled = () => wait('findControl("agent-models-refresh-providers")?.semantics?.state?.disabled === false');
  const control = async (id, action='activate', value='') => {
    try {
      await wait(`!!findControl(${JSON.stringify(id)}) && !findControl(${JSON.stringify(id)}).semantics?.state?.disabled`);
    } catch(error) {
      const controls = await tab.evaluate(`(()=>{const out=[];const walk=n=>{if(!n)return;if(n.test_id)out.push({id:n.test_id,widget:n.widget,rect:n.rect});(n.children??[]).forEach(walk)};walk(mount.state()?.semantics?.root);return out;})()`);
      throw new Error(`${error.message}; production controls: ${JSON.stringify(controls)}; runtime: ${await tab.evaluate('JSON.stringify({phase:mount.state()?.phase,error:mount.state()?.error,frames:mount.state()?.frames})')}; browser errors: ${tab.log.join('; ')}; browser: ${browser.diagnostics()}`, {cause:error});
    }
    // Catch inside the page so CDP never includes the expression (and a
    // private editor's input) in its exception diagnostics.
    const result = await tab.evaluate(`(()=>{try{control(${JSON.stringify(id)},${JSON.stringify(action)},${JSON.stringify(value)});return {ok:true};}catch(error){return {ok:false,type:typeof error,name:error instanceof Error?error.name:null};}})()`);
    assert(result.ok, `production control ${id} ${action} failed: ${JSON.stringify(result)}`);
    // Await the production presentation after each edit. The next control
    // must use its current owner path, and Save must see synchronized buffers.
    const before = await tab.evaluate('semanticDeliveries');
    await tab.evaluate('mount.requestSemantics()');
    await wait(`semanticDeliveries > ${before}`);
    if (action === 'value' && !id.startsWith('agent-models-api-key-')) {
      await wait(`findControl(${JSON.stringify(id)})?.semantics?.value === ${JSON.stringify(value)}`);
    }
  };
  const noSecrets = async () => {
    const before = await tab.evaluate('semanticDeliveries');
    await tab.evaluate('mount.requestSemantics()');
    await wait(`semanticDeliveries > ${before}`);
    assert(!(await tab.evaluate('privateSurface()')).includes(secret), 'secret leaked to resource metadata, document or UI snapshot');
    assert(!JSON.stringify(config).includes(secret), 'secret leaked to backend configuration');
  };
  assert.equal(await tab.evaluate('mount.state()?.phase'), 'presented',
    await tab.evaluate('JSON.stringify({phase:mount.state()?.phase,error:mount.state()?.error,frames:mount.state()?.frames})') + '; ' + tab.log.join('; ') + '; browser: ' + browser.diagnostics());
  assert((await tab.screenshot())?.length > 1000, 'production WebGPU canvas was not captured');
  await settled();
  await control('agent-models-add-openai');
  await wait('mount.actions().some(a=>a.resource_kind==="settings.agent-provider")');
  assert.equal(config.profiles.length, 1);
  const id = config.profiles[0].id;
  await control(`agent-models-label-${id}`, 'value', 'Browser managed provider');
  await control(`agent-models-url-${id}`, 'value', 'https://provider.example/v1');
  await control(`agent-models-model-${id}`, 'value', 'browser-model');
  await control(`agent-models-save-provider-${id}`);
  await backend(() => config.profiles[0]?.label === 'Browser managed provider' && config.profiles[0]?.default_model === 'browser-model');
  await settled();
  await wait(`findControl(${JSON.stringify(`agent-models-default-${id}`)})`);
  assert.equal(config.profiles[0].label, 'Browser managed provider');
  assert.equal(config.profiles[0].default_model, 'browser-model');
  await control(`agent-models-default-${id}`);
  await backend(() => config.default_profile === id);
  await settled();
  assert.equal(config.default_profile, id);
  await control(`agent-models-api-key-${id}`, 'value', secret);
  await noSecrets();
  rejectKey = true;
  await control(`agent-models-set-api-key-${id}`);
  await backend(() => !!releaseFailure);
  await wait('findControl("agent-models-refresh-providers")?.semantics?.state?.disabled === true');
  await noSecrets();
  assert(!credentials.has(id));
  releaseFailure();
  releaseFailure = undefined;
  await settled();
  await noSecrets();
  // No reinsertion: retry must read the same retained private widget buffer.
  await control(`agent-models-set-api-key-${id}`);
  await wait(`JSON.stringify(mount.state()?.semantics).includes('API key is stored')`);
  assert.equal(credentials.get(id), secret);
  await noSecrets();

  // Hold a real HTTP response after the production save click. The editor must
  // retain the newer draft, both while pending and after the failure arrives.
  rejectWrite = true;
  await control(`agent-models-label-${id}`, 'value', 'Retained failed draft');
  await control(`agent-models-save-provider-${id}`);
  await backend(() => !!releaseFailure);
  await wait('findControl("agent-models-refresh-providers")?.semantics?.state?.disabled === true');
  await wait(`JSON.stringify(findControl(${JSON.stringify(`agent-models-label-${id}`)})).includes('Retained failed draft')`);
  assert(releaseFailure, 'production save never reached HTTPS backend');
  assert.equal(config.profiles[0].label, 'Browser managed provider');
  releaseFailure();
  releaseFailure = undefined;
  await settled();
  await wait(`JSON.stringify(findControl(${JSON.stringify(`agent-models-label-${id}`)})).includes('Retained failed draft')`);
  await noSecrets();

  const confirmed = async label => {
    await tab.evaluate(`(()=>{const a=mount.actions().find(a=>a.resource_kind==='settings.agent-provider'&&a.action_label===${JSON.stringify(label)});if(!a)throw Error('missing '+${JSON.stringify(label)});return mount.invokeAction(a.address.resource,a.address.action,{}, {confirm:true});})()`);
  };
  await confirmed('Remove API key');
  await backend(() => !credentials.has(id));
  await settled();
  assert(!credentials.has(id));
  await wait(`!JSON.stringify(mount.state()?.semantics).includes('API key is stored')`);
  await confirmed('Remove provider');
  await backend(() => config.profiles.length === 0);
  await wait('!mount.actions().some(a=>a.resource_kind==="settings.agent-provider")');
  assert.equal(config.profiles.length, 0);
  await noSecrets();
  assert(calls.some(c=>c.verb==='credential_status'));
  assert(calls.filter(c=>c.verb==='set_provider_config').length >= 5);
  assert(!tab.log.some(line=>line.startsWith('EXC') || /panic/i.test(line)), tab.log.join('\n'));
  await mountCleanup(tab);
  console.log('PASS Model Providers production WASM controls, HTTPS administration, private secrets and retained failed draft');
} finally {
  releaseFailure?.();
  try {
    await browser?.close();
  } finally {
    if(server) await new Promise(resolve=>server.close(resolve));
  }
}
async function mountCleanup(tab) {await tab.evaluate('mount.cleanup()');}
