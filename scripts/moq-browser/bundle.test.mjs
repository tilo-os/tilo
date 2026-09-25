import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { init, parse } from "es-module-lexer";
import * as Library from "../../crates/browser/qos-browser-runtime/web/media-moq-library.mjs";

const ARTIFACT = "../../crates/browser/qos-browser-runtime/web/media-moq-library.mjs";
const MANIFEST = "../../crates/browser/qos-browser-runtime/web/media-moq-library.provenance.json";
const LICENSES = "../../crates/browser/qos-browser-runtime/web/media-moq-library.LICENSE.txt";

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function rebuild() {
  const result = spawnSync(process.execPath, ["build.mjs"], {
    cwd: import.meta.dirname,
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
}

async function bareImports(source) {
  await init;
  const [imports] = parse(source);
  return imports
    .map((entry) => entry.specifier)
    .filter((specifier) => specifier && !specifier.startsWith(".") && !specifier.startsWith("/"));
}

test("bundle exports the upstream connection and media role namespaces", () => {
  assert.equal(typeof Library.Connection.connect, "function");
  assert.equal(typeof Library.Broadcast.Producer, "function");
  assert.equal(typeof Library.Track.Producer, "function");
});

test("bundle rebuild is byte reproducible and records its exact digest", async () => {
  rebuild();
  const firstArtifact = await readFile(new URL(ARTIFACT, import.meta.url));
  const firstManifest = await readFile(new URL(MANIFEST, import.meta.url));

  rebuild();
  const secondArtifact = await readFile(new URL(ARTIFACT, import.meta.url));
  const secondManifest = await readFile(new URL(MANIFEST, import.meta.url));

  assert.deepEqual(secondArtifact, firstArtifact);
  assert.deepEqual(secondManifest, firstManifest);

  const manifest = JSON.parse(secondManifest);
  assert.equal(manifest.upstream.commit, "3aedaf3503146c664479f6fc53a50b36bb5e816d");
  assert.equal(manifest.protocol.alpn, "moqt-21");
  assert.equal(manifest.upstream.registryPackageAccepted, false);
  assert.equal(manifest.artifact.sha256, sha256(secondArtifact));
  assert.deepEqual(manifest.artifact.externalImports, []);
});

test("bare import checker detects an unresolved package import", async () => {
  assert.deepEqual(await bareImports('import "unresolved-package";'), ["unresolved-package"]);
});

test("deployed bundle has no bare imports and carries its license closure", async () => {
  const artifact = await readFile(new URL(ARTIFACT, import.meta.url), "utf8");
  const licenses = await readFile(new URL(LICENSES, import.meta.url), "utf8");

  assert.deepEqual(await bareImports(artifact), []);
  assert.match(artifact, /MIT OR Apache-2\.0/);
  assert.match(artifact, /BUNDLED LICENSES FOR media-moq-library\.mjs/);
  assert.match(artifact, /Apache License\s+Version 2\.0/);
  for (const dependency of [
    "@moq/net source",
    "@moq/qmux@0.3.3",
    "@moq/web-socket-stream@0.1.1",
    "@zod/mini@4.5.0",
    "async-mutex@0.5.0",
    "bowser@2.14.1",
    "tslib@2.8.1",
  ]) {
    assert.match(licenses, new RegExp(dependency.replaceAll("@", "\\@")));
  }
});
