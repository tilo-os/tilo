/**
 * The verifier `registry-client.mjs` uses, backed by the `registry-web` Rust
 * crate through `registry-web-bindgen` (build it with `build-verifier.sh`).
 *
 * Interface (any object with these methods works, e.g. a fake in tests):
 *
 *   verifySnapshot({bytes, ownerKeyHex, checkpoint, nowUnix})
 *     -> {sequence, checkpoint: Uint8Array, select(packageId, {features, runtime}) -> Selection}
 *   verifyPackage({bytes, selection})
 *     -> {packageId, version, kind: 'app'|'runtime', runtime, packageRoot,
 *         module: {path, blake3}, assets: [{name, blake3}], files: [{path, blake3}]}
 *   blake3Hex(bytes) -> string
 *
 * A Selection is `{packageId, version, packageRoot, runtime, json}`; `json`
 * is what the Rust side binds the package to.
 *
 *   import init, * as bindings from './pkg/registry_verifier.js';
 *   await init();
 *   const verifier = createVerifier(bindings);
 */
export function createVerifier(bindings) {
  const selection = json => {
    const raw = JSON.parse(json);
    return {
      packageId: raw.package_id,
      version: raw.version,
      packageRoot: raw.package_root_hex,
      runtime: raw.target.runtime,
      json,
    };
  };
  const file = ({path, blake3_hex: blake3}) => ({path, blake3});
  return {
    verifySnapshot({bytes, ownerKeyHex, checkpoint, nowUnix}) {
      const snapshot = bindings.verifySnapshot(bytes, ownerKeyHex, checkpoint ?? undefined, BigInt(nowUnix));
      return {
        sequence: Number(snapshot.sequence()),
        checkpoint: snapshot.checkpoint(),
        select: (packageId, {features, runtime} = {}) => selection(snapshot.select(packageId, [...features], runtime ?? undefined)),
      };
    },
    verifyPackage({bytes, selection: chosen}) {
      const pkg = JSON.parse(bindings.verifyPackage(bytes, chosen.json));
      return {
        packageId: pkg.package_id,
        version: pkg.version,
        kind: pkg.kind,
        runtime: pkg.runtime,
        packageRoot: pkg.package_root_hex,
        module: file(pkg.module),
        assets: pkg.assets.map(({path: name, blake3_hex: blake3}) => ({name, blake3})),
        files: pkg.files.map(file),
      };
    },
    blake3Hex: bytes => bindings.blake3Hex(bytes),
  };
}
