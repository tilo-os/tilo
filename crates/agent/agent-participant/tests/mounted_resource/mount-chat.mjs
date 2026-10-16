/** Mount the real compiled Chat with existing certified fixture custody. */
export async function mountChat(tab) {
  return await tab.evaluate(`(async () => { try {
    const host = await import('/host.mjs');
    window.runtime = await host.loadRuntime();
    const {profileStore, profileDevice} = await import('/peer.mjs');
    const {idbStorage} = await import('/device-custody.mjs');
    const storage = idbStorage();
    for (const [root, bytes] of [[fixture.root, fixture.accounts],
      [fixture.decoy, fixture.decoyAccounts]]) {
      const profile = 'tilo-identity:' + root + '@' + new URL(fixture.origin).host;
      // Seed the namespace the entry page actually discovers, including when
      // a profile has moved after its host was set up again.
      const device = profileDevice(profile);
      const opened = await host.openProfileStorage(profile, device);
      await opened.lease.commit([{key: runtime.browser_accounts_key(root), bytes: new Uint8Array(bytes), revision: '0'}]);
      if (root === fixture.root) {
        await profileStore(runtime, profile, device).put('certificate', JSON.stringify(fixture.certificate));
        const wrapKey = await crypto.subtle.generateKey({name:'AES-GCM', length:256}, false, ['encrypt','decrypt']);
        const iv = crypto.getRandomValues(new Uint8Array(12));
        const sealed = new Uint8Array(await crypto.subtle.encrypt({name:'AES-GCM', iv}, wrapKey, new Uint8Array(fixture.seed)));
        await storage.put('device-key:' + profile, {kind:'sealed-seed', wrapKey, iv, sealed});
      }
      await opened.release();
    }
    const {readBrowserAccounts, currentBrowserAccount} = await import('/browser-accounts.mjs');
    window.readAccounts = () => readBrowserAccounts({origin: fixture.origin, runtime, currentDevice: profileDevice});
    const accounts = await readAccounts();
    if (accounts.length !== 2) throw Error('two actual Settings account choices required');
    window.selected = accounts.find(account => account.userPublicKey === fixture.root);
    await currentBrowserAccount(selected, readAccounts);
    const {connectPeer, withCustody} = await import('/peer.mjs');
    const {openDeviceCustody} = await import('/device-custody.mjs');
    const custody = await openDeviceCustody({profile:selected.profile, storage, requireExisting:true});
    window.mountDiagnostic = [];
    const desktop = {...host, mountDesktop:(target, options)=>host.mountDesktop(target, {...options,
      onStatus:status=>{if(status.catalogueError || status.clipboard) mountDiagnostic.push(status); options.onStatus?.(status);}})};
    let voiceOptions;
    if (fixture.voiceJourney) {
      const {createBrowserAudio} = await import('/voice-audio.mjs');
      window.voiceEvidence = {contexts:[], streams:[], workers:[], levels:{}, reports:[]};
      const environment = {
        AudioContext: class extends AudioContext {constructor(options) {super(options); voiceEvidence.contexts.push(this);}},
        AudioWorkletNode, AudioEncoder, AudioDecoder, AudioData, EncodedAudioChunk,
        Worker: class extends Worker {
          constructor(...args) {super(...args); voiceEvidence.workers.push(this);}
          terminate() {this.stopped = true; super.terminate();}
        },
        navigator: {mediaDevices: {async getUserMedia(options) {
          const stream = await navigator.mediaDevices.getUserMedia(options);
          voiceEvidence.streams.push(stream); return stream;
        }}},
      };
      const factory = createBrowserAudio({environment});
      voiceOptions = {audio: {start(options) {
        return factory.start({...options, onLevels(levels) {
          for (const [publisher, rms] of levels.publishers) {
            if (rms > 0.015) voiceEvidence.levels[publisher] = (voiceEvidence.levels[publisher] ?? 0) + 1;
          }
          options.onLevels(levels);
        }});
      }}, onReport: report => voiceEvidence.reports.push(report.type)};
    }
    window.peer = await withCustody(custody, () => connectPeer({origin:fixture.origin,
      room:fixture.room, userPublicKey:selected.userPublicKey, profile:selected.profile,
      device:selected.device, custody, runtime, desktop, voice:voiceOptions, live:false, requireExistingCertificate:true}));
    await peer.whenReady();
    window.catalogueDiagnostic = {calls:0, replies:0};
    const catalogue = peer.actions.catalogue.bind(peer.actions);
    peer.actions.catalogue = async query => {
      catalogueDiagnostic.query = query;
      catalogueDiagnostic.calls++;
      try { const page = await catalogue(query); catalogueDiagnostic.replies++; catalogueDiagnostic.rows=page.rows.length; catalogueDiagnostic.locator=Boolean(page.locator); return page; }
      catch (error) { catalogueDiagnostic.error = String(error?.message ?? error); throw error; }
    };
    // What a followed link does on the full-tab entry page: mount the app on
    // the shared record, then show the resource through its declared view.
    // The link's record segment is the publishing record (the provider is
    // record:<app>:<record>, empty for the package's primary record).
    const {kind, id, span} = fixture.reference;
    const prefix = 'record:so.tilo.chat:';
    if (!fixture.provider.startsWith(prefix)) throw Error('fixture provider is not a Chat record');
    const record = fixture.provider.slice(prefix.length);
    const target = {app:'so.tilo.chat', record, resource:{kind, id, ...(span ? {span} : {})}};
    await peer.whenRecord(target.app, record);
    window.mount = await peer.mountDesktop(document.querySelector('#surface'), {app:target.app, record:record || '@tilo:package-primary'});
    const {openTarget} = await import('/open-target.mjs');
    await openTarget({mount, target, mode:'app'});
    await currentBrowserAccount(selected, readAccounts);
    mount.requestSemantics();
    return {actions:mount.actions(fixture.reference.id)};
  } catch (error) {
    return {error:String(error?.message ?? error), stack:error?.stack};
  }
  })()`);
}
