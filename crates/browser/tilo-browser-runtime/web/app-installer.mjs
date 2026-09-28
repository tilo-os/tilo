import {RuntimeAbiMismatch, compileModule} from './dylink.mjs';

/**
 * The short spellings the Cloud frontend passes to a standalone mount, as
 * `ALIASES` in src/catalog.rs spells them. A resolver is always asked for the
 * full app id.
 */
export const APP_ALIASES = Object.freeze({writer: 'so.tilo.word-processor', chat: 'so.tilo.chat'});
export const canonicalAppId = app => Object.hasOwn(APP_ALIASES, app) ? APP_ALIASES[app] : app;

/**
 * A local desktop's seed (`MountOptions.local`) with every app named by its
 * full id, and the apps it needs linked: its windows' and the `apps` its
 * launcher should list.
 */
export function localSeed(local) {
  if (!local || typeof local !== 'object') throw TypeError('local must be an object');
  const apps = new Set((local.apps ?? []).map(canonicalAppId));
  const workspaces = (local.workspaces ?? []).map(workspace => ({
    ...workspace,
    strips: (workspace.strips ?? []).map(strip => ({
      ...strip,
      windows: (strip.windows ?? []).map(window => {
        const app = canonicalAppId(window.app);
        apps.add(app);
        return {...window, app};
      }),
    })),
  }));
  return {seed: {...local, apps: [...apps], workspaces}, apps: [...apps]};
}

/**
 * Installs apps the core does not contain from side modules a resolver
 * supplies (WEB_RUNTIME.md).
 *
 * `resolveApp(appId, {signal})` returns (or resolves to)
 * `{module, abi, assets}`: `module` a WebAssembly.Module, a Response or the
 * verified bytes of the app's side module, `abi` the runtime ABI id it was
 * built for, `assets` a Map from the app's asset names to URLs. The
 * registry client's `resolveApp` has this shape.
 *
 * Resolution and compilation start at once, alongside the core's own
 * download; only linking waits for the core.
 */
export function createAppInstaller({ready, installed, canonical = canonicalAppId, linker, onLinked = () => {}}) {
  let defaultResolver;
  const jobs = new Map(), assets = new Map();

  function unavailable(appId, cause) {
    const error = Error(`application ${JSON.stringify(appId)} is not available${cause ? `: ${cause.message ?? cause}` : ''}`, cause ? {cause} : undefined);
    error.code = 'TILO_APP_UNAVAILABLE';
    error.app = appId;
    return error;
  }

  async function link(appId, resolver, signal, onStatus) {
    const core = ready();
    let resolution;
    if (resolver) {
      onStatus?.({phase: 'resolving', app: appId});
      resolution = Promise.resolve().then(() => resolver(appId, {signal})).then(async resolved => {
        if (!resolved?.module) throw Error('the resolver returned no module');
        return {...resolved, module: await compileModule(resolved.module)};
      });
      // A compiled-in app never needs the resolver's answer, or its failure.
      resolution.catch(() => {});
    }
    await core;
    if (installed(appId)) return;
    if (!resolution) throw unavailable(appId);
    let resolved;
    try {resolved = await resolution;} catch (error) {throw unavailable(appId, error);}
    const active = linker();
    // A manifest naming another ABI fails here; the module's own section is
    // checked again before it is instantiated.
    if (resolved.abi !== undefined && resolved.abi !== active.abi) throw new RuntimeAbiMismatch(appId, active.abi, resolved.abi);
    onStatus?.({phase: 'linking', app: appId});
    const exports = await active.instantiate(resolved.module, appId);
    if (typeof exports.tilo_app_register !== 'function') throw Error(`${appId}: the side module exports no tilo_app_register`);
    const code = exports.tilo_app_register();
    if (!installed(appId)) {
      throw Error(code === 1
        ? `${appId}: the module's app id is already installed from another module`
        : `${appId}: the module registered a different app, or none (code ${code})`);
    }
    assets.set(appId, new Map(resolved.assets ?? []));
    onLinked(appId);
    onStatus?.({phase: 'linked', app: appId});
  }

  function waitable(job, signal) {
    if (!signal) return job;
    if (signal.aborted) return Promise.reject(signal.reason ?? Error('aborted'));
    return new Promise((resolve, reject) => {
      const abort = () => reject(signal.reason ?? Error('aborted'));
      signal.addEventListener('abort', abort, {once: true});
      job.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort));
    });
  }

  /** Make `app` (an id or alias) installed, resolving and linking it at most once. */
  function ensure(app, {resolveApp, signal, onStatus} = {}) {
    const appId = canonical(app);
    let job = jobs.get(appId);
    if (!job) {
      job = link(appId, resolveApp ?? defaultResolver, signal, onStatus);
      jobs.set(appId, job);
      // A failed attempt may be retried, for example once the network is back.
      job.catch(() => {if (jobs.get(appId) === job) jobs.delete(appId);});
    }
    return waitable(job, signal);
  }

  /**
   * Resolve and link several apps concurrently, as a session restore does
   * for every app it will open. Resolves to the ids when all are installed;
   * otherwise rejects with an AggregateError whose `failed` maps each app that
   * is not to its error (the others are installed).
   */
  async function preload(apps, options = {}) {
    const ids = [...new Set(apps.map(canonical))];
    const results = await Promise.allSettled(ids.map(id => ensure(id, options)));
    const failed = new Map(results.flatMap((result, index) => result.status === 'rejected' ? [[ids[index], result.reason]] : []));
    if (failed.size) {
      const error = new AggregateError([...failed.values()], `could not install ${[...failed.keys()].join(', ')}`);
      error.failed = failed;
      error.installed = ids.filter(id => !failed.has(id));
      throw error;
    }
    return ids;
  }

  return {
    ensure,
    preload,
    setResolver(resolver) {defaultResolver = resolver;},
    assets: appId => assets.get(canonical(appId)),
    /** Where any linked app's resolver placed the asset `name`. */
    asset: name => {
      for (const named of assets.values()) if (named.has(name)) return named.get(name);
    },
  };
}
