/**
 * Resolves apps (and the toolkit core) for the browser runtime from a signed
 * Tilo registry. Same `resolveApp(appId, {signal})` shape as
 * tilo-browser-runtime's `web/app-directory.mjs`, so it plugs into
 * `host.mjs`'s `setAppResolver` (see WEB_RUNTIME.md there).
 *
 * Trust: the page pins the registry owner's Ed25519 key. Every byte is
 * checked by the verifier (`registry-verifier.mjs`, the `registry-web` Rust
 * crate) before it is returned: the snapshot's signatures and freshness plus
 * an anti-rollback checkpoint, each tapp against the version selected from
 * that snapshot, each payload blob against its BLAKE3.
 *
 * Network, per page load:
 *   GET /entries?format=compact    always, `cache: 'no-cache'`: the browser
 *                                  revalidates with the ETag, 304 when unchanged.
 *                                  Per entry only the newest versions of each
 *                                  target; a registry without the compact form
 *                                  answers with the full snapshot, which
 *                                  verifies the same way.
 *   GET /thin/<package root>.tapp  for every package not yet cached, concurrently
 *   GET /blobs/<blake3>            for every missing module/asset blob of the
 *                                  whole batch, concurrently
 * Thin tapps and blobs are immutable and live in Cache Storage keyed by
 * their URL, so a returning visit costs one small snapshot request, and a
 * module comes back as the cached Response (its URL is the blob's, so
 * `WebAssembly.compileStreaming` can reuse compiled code). Nothing uses
 * localStorage.
 */

/** What rustc enables by default for wasm32-unknown-unknown; every browser that runs the core offers it. */
export const DEFAULT_WASM_FEATURES = Object.freeze([
  'bulk-memory', 'bulk-memory-opt', 'call-indirect-overlong', 'multivalue',
  'mutable-globals', 'nontrapping-fptoint', 'reference-types', 'sign-ext',
]);

export const DEFAULT_RUNTIME_PACKAGE = 'so.tilo.runtime.web';

const WASM = 'application/wasm';

function codedError(message, code, cause) {
  const error = Error(message, cause ? {cause} : undefined);
  error.code = code;
  return error;
}

/** Settle with `promise`, or reject as soon as `signal` aborts; the shared work goes on. */
function abortable(promise, signal) {
  if (!signal) return promise;
  if (signal.aborted) return Promise.reject(signal.reason ?? Error('aborted'));
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason ?? Error('aborted'));
    signal.addEventListener('abort', abort, {once: true});
    promise.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort));
  });
}

/**
 * @param {object} options
 * @param {string} options.baseUrl registry base URL, e.g. `https://apps.tilo.so`
 * @param {string} options.ownerKeyHex the pinned registry owner key (64 hex)
 * @param {object} options.verifier see `registry-verifier.mjs`
 * @param {string} [options.cacheName] Cache Storage bucket
 * @param {string} [options.runtimeAbi] ABI id of the loaded core; apps are selected for exactly it
 * @param {string[]} [options.features] wasm features the browser offers
 * @param {string} [options.runtimePackage] package id of the toolkit core
 */
export function createRegistryClient({
  baseUrl,
  ownerKeyHex,
  verifier,
  cacheName = 'tilo-registry',
  runtimeAbi,
  features = DEFAULT_WASM_FEATURES,
  runtimePackage = DEFAULT_RUNTIME_PACKAGE,
  fetch: fetchResource = globalThis.fetch?.bind(globalThis),
  caches: cacheStorage = globalThis.caches,
  now = () => Date.now(),
  createObjectURL = globalThis.URL?.createObjectURL,
} = {}) {
  if (!baseUrl || !/^[0-9a-f]{64}$/i.test(ownerKeyHex ?? '') || !verifier) {
    throw TypeError('createRegistryClient needs baseUrl, a 64-hex ownerKeyHex and a verifier');
  }
  const base = String(baseUrl).replace(/\/+$/, '');
  const checkpointKey = `${base}/.tilo/checkpoint/${ownerKeyHex.toLowerCase()}`;
  let abi = runtimeAbi;
  let snapshot;
  let cache;
  let queued;
  const resolving = new Map();
  // Verified blobs of a page without Cache Storage (e.g. an insecure context).
  const memory = new Map();

  function openCache() {
    if (!cache) {
      cache = cacheStorage ? cacheStorage.open(cacheName).catch(() => null) : Promise.resolve(null);
    }
    return cache;
  }

  async function fetchOk(url, init) {
    const response = await fetchResource(url, {credentials: 'omit', ...init});
    if (!response.ok) throw codedError(`${url}: HTTP ${response.status}`, 'TILO_REGISTRY_HTTP');
    return response;
  }

  async function readCheckpoint() {
    const stored = await (await openCache())?.match(checkpointKey);
    return stored ? new Uint8Array(await stored.arrayBuffer()) : null;
  }

  async function loadSnapshot() {
    const response = await fetchOk(`${base}/entries?format=compact`, {cache: 'no-cache'});
    const bytes = new Uint8Array(await response.arrayBuffer());
    const checkpoint = await readCheckpoint();
    let verified;
    try {
      verified = await verifier.verifySnapshot({bytes, ownerKeyHex, checkpoint, nowUnix: Math.floor(now() / 1000)});
    } catch (error) {
      throw codedError(`registry snapshot refused: ${error.message ?? error}`, 'TILO_REGISTRY_UNVERIFIED', error);
    }
    await (await openCache())?.put(checkpointKey, new Response(verified.checkpoint));
    return verified;
  }

  /** The verified snapshot of this page load; retried after a failure. */
  function currentSnapshot() {
    if (!snapshot) {
      const attempt = loadSnapshot();
      snapshot = attempt;
      attempt.catch(() => {if (snapshot === attempt) snapshot = undefined;});
    }
    return snapshot;
  }

  /** Immutable bytes by URL: Cache Storage first, else one network fetch. */
  async function cachedBytes(url) {
    const hit = await (await openCache())?.match(url);
    return hit ? new Uint8Array(await hit.arrayBuffer()) : null;
  }

  async function fetchThin(selection) {
    const url = `${base}/thin/${selection.packageRoot}.tapp`;
    let bytes = await cachedBytes(url);
    let fresh;
    if (!bytes) {
      fresh = await fetchOk(url, {cache: 'no-store'});
      bytes = new Uint8Array(await fresh.clone().arrayBuffer());
    }
    let verified;
    try {
      verified = await verifier.verifyPackage({bytes, selection});
    } catch (error) {
      throw codedError(`${selection.packageId}: package refused: ${error.message ?? error}`, 'TILO_REGISTRY_UNVERIFIED', error);
    }
    if (fresh) await (await openCache())?.put(url, fresh);
    return verified;
  }

  /** Make one blob available: cached, or fetched, verified and then cached under its URL. */
  async function fetchBlob(hash) {
    const store = await openCache();
    const url = `${base}/blobs/${hash}`;
    if (memory.has(hash) || await store?.match(url)) return;
    const response = await fetchOk(url, {cache: 'no-store'});
    const copy = response.clone();
    const bytes = new Uint8Array(await response.arrayBuffer());
    if ((await verifier.blake3Hex(bytes)) !== hash) {
      throw codedError(`blob ${hash} does not match its hash`, 'TILO_REGISTRY_UNVERIFIED');
    }
    if (store) await store.put(url, copy);
    else memory.set(hash, bytes);
  }

  async function blob(hash) {
    const url = `${base}/blobs/${hash}`;
    const hit = await (await openCache())?.match(url);
    if (hit) return hit;
    const bytes = memory.get(hash);
    if (!bytes) throw codedError(`blob ${hash} was not fetched`, 'TILO_REGISTRY_HTTP');
    const wasm = bytes[0] === 0 && bytes[1] === 0x61 && bytes[2] === 0x73 && bytes[3] === 0x6d;
    return new Response(bytes, {headers: {'content-type': wasm ? WASM : 'application/octet-stream'}});
  }

  async function assetUrl(hash) {
    const response = await blob(hash);
    if (!createObjectURL) return `${base}/blobs/${hash}`;
    return createObjectURL(await response.blob());
  }

  async function asResult(verified) {
    const module = await blob(verified.module.blake3);
    const assets = new Map(await Promise.all(verified.assets.map(async asset => [asset.name, await assetUrl(asset.blake3)])));
    return {module, abi: verified.runtime, assets};
  }

  /**
   * One batch: select every package from one snapshot, fetch the thin tapps
   * concurrently, then every missing blob concurrently. Settles each
   * package's own promise.
   */
  async function runBatch(requests) {
    let verified;
    try {verified = await currentSnapshot();} catch (error) {
      for (const request of requests) request.reject(error);
      return;
    }
    const packages = await Promise.all(requests.map(async request => {
      try {
        const selection = await verified.select(request.packageId, {features, runtime: request.runtime});
        const pkg = await fetchThin(selection);
        if (pkg.kind !== request.kind) throw codedError(`${request.packageId} is a ${pkg.kind}, not a ${request.kind}`, 'TILO_REGISTRY_UNVERIFIED');
        return pkg;
      } catch (error) {
        if (!error.code) error.code = 'TILO_APP_UNAVAILABLE';
        request.reject(error);
        return null;
      }
    }));
    // Every blob of the batch starts now, once, however many packages share it.
    const blobs = new Map();
    const needs = pkg => [pkg.module.blake3, ...pkg.assets.map(asset => asset.blake3)];
    for (const hash of packages.flatMap(pkg => pkg ? needs(pkg) : [])) {
      if (!blobs.has(hash)) {
        const job = fetchBlob(hash);
        job.catch(() => {});
        blobs.set(hash, job);
      }
    }
    await Promise.all(requests.map(async (request, index) => {
      const pkg = packages[index];
      if (!pkg) return;
      try {
        await Promise.all(needs(pkg).map(hash => blobs.get(hash)));
        request.resolve(await asResult(pkg));
      } catch (error) {
        request.reject(error);
      }
    }));
  }

  /** Queue a package for the next batch; concurrent calls in one task share it. */
  function enqueue(packageId, kind, runtime) {
    const key = `${kind}\0${packageId}\0${runtime ?? ''}`;
    let job = resolving.get(key);
    if (!job) {
      job = new Promise((resolve, reject) => {
        if (!queued) {
          queued = [];
          setTimeout(() => {const batch = queued; queued = undefined; runBatch(batch);}, 0);
        }
        queued.push({packageId, kind, runtime, resolve, reject});
      });
      resolving.set(key, job);
      // Settled results are not memoized: the host installs a module once,
      // and a failed attempt may be retried.
      job.then(() => resolving.delete(key), () => resolving.delete(key));
    }
    return job;
  }

  /** `(appId, {signal}) => {module, abi, assets}`: verified, for `setAppResolver`. */
  function resolveApp(appId, {signal} = {}) {
    return abortable(enqueue(appId, 'app', abi), signal);
  }

  /**
   * Several apps in one batch: one snapshot, their thin tapps concurrently,
   * then all missing blobs concurrently. Resolves to the results in order;
   * otherwise rejects with an AggregateError whose `failed` maps each app
   * id to its error and `resolved` the others to their results.
   */
  async function resolveApps(appIds, {signal} = {}) {
    const ids = [...new Set(appIds)];
    const settled = await abortable(Promise.allSettled(ids.map(id => enqueue(id, 'app', abi))), signal);
    const failed = new Map(settled.flatMap((result, index) => result.status === 'rejected' ? [[ids[index], result.reason]] : []));
    if (failed.size) {
      const error = new AggregateError([...failed.values()], `could not resolve ${[...failed.keys()].join(', ')}`);
      error.failed = failed;
      error.resolved = new Map(settled.flatMap((result, index) => result.status === 'fulfilled' ? [[ids[index], result.value]] : []));
      throw error;
    }
    const byId = new Map(settled.map((result, index) => [ids[index], result.value]));
    return appIds.map(id => byId.get(id));
  }

  /**
   * The toolkit core, `{module, abi, assets}`: for exactly `abi` when given
   * (or configured), else the newest the browser runs. Apps resolved
   * afterwards are selected for its ABI.
   */
  async function resolveRuntime({abi: wanted = abi, signal} = {}) {
    const result = await abortable(enqueue(runtimePackage, 'runtime', wanted), signal);
    abi = result.abi;
    return result;
  }

  return {
    resolveApp,
    resolveApps,
    resolveRuntime,
    /** The verified snapshot's sequence number. */
    sequence: async () => (await currentSnapshot()).sequence,
  };
}
