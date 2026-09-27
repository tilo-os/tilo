/**
 * Resolves apps from a plain directory laid out by build-web-app.sh, for
 * local development without a registry:
 *
 *   apps/index.json            {"schema":1,"apps":{"<app id>":"apps/<app id>/app.json"}}
 *   apps/<app id>/app.json     {id, abi, module:{path,bytes,sha256}, assets:{name:path}}
 *   apps/<app id>/app.wasm
 *
 * Same shape as the registry client: `resolveApp(appId, {signal})` resolves to
 * `{module, abi, assets}`. The module is checked against the manifest's size
 * and SHA-256; nothing here is signed, so it is no substitute for a registry.
 */
const hex = bytes => [...new Uint8Array(bytes)].map(value => value.toString(16).padStart(2, '0')).join('');

export function createDirectoryResolver({baseURL, fetch: fetchAsset = globalThis.fetch, crypto = globalThis.crypto} = {}) {
  const root = new URL(baseURL ?? './', globalThis.location?.href ?? import.meta.url);
  let index;

  async function fetchOk(url, signal) {
    const response = await fetchAsset(url, {signal, credentials: 'same-origin'});
    if (!response.ok) throw Error(`${url} HTTP ${response.status}`);
    return response;
  }
  function apps(signal) {
    if (!index) {
      const attempt = fetchOk(new URL('apps/index.json', root), signal).then(r => r.json());
      index = attempt;
      attempt.catch(() => {if (index === attempt) index = undefined;});
    }
    return index;
  }

  async function resolveApp(appId, {signal} = {}) {
    const listed = (await apps(signal)).apps?.[appId];
    if (!listed) throw Error(`${appId} is not in ${new URL('apps/index.json', root)}`);
    const manifestURL = new URL(listed, root);
    const manifest = await (await fetchOk(manifestURL, signal)).json();
    if (manifest.schema !== 1 || manifest.id !== appId || typeof manifest.abi !== 'string') throw Error(`${manifestURL} does not describe ${appId}`);
    const moduleURL = new URL(manifest.module.path, manifestURL);
    const bytes = new Uint8Array(await (await fetchOk(moduleURL, signal)).arrayBuffer());
    if (bytes.byteLength !== manifest.module.bytes || hex(await crypto.subtle.digest('SHA-256', bytes)) !== manifest.module.sha256) {
      throw Error(`${moduleURL} does not match its manifest`);
    }
    const assets = new Map(Object.entries(manifest.assets ?? {}).map(([name, path]) => [name, new URL(path, manifestURL).href]));
    return {module: bytes, abi: manifest.abi, assets};
  }

  return {resolveApp, resolveApps: (appIds, options) => Promise.all(appIds.map(appId => resolveApp(appId, options)))};
}
