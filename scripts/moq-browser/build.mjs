import { createHash } from "node:crypto";
import { promises as fs } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, "../..");
const VENDOR = path.join(ROOT, "vendor/moq/js");
const OUTPUT = path.join(
  ROOT,
  "crates/browser/qos-browser-runtime/web/media-moq-library.mjs",
);
const MANIFEST = path.join(
  ROOT,
  "crates/browser/qos-browser-runtime/web/media-moq-library.provenance.json",
);
const LICENSE_BUNDLE = path.join(
  ROOT,
  "crates/browser/qos-browser-runtime/web/media-moq-library.LICENSE.txt",
);
const LOCK = path.join(SCRIPT_DIR, "package-lock.json");

const UPSTREAM_COMMIT = "3aedaf3503146c664479f6fc53a50b36bb5e816d";
const PATCHES = [
	"net/src/broadcast.ts",
	"net/src/connection/connect.ts",
	"net/src/connection/handshake.ts",
	"net/src/group.ts",
	"net/src/ietf/aliases.ts",
	"net/src/ietf/connection.ts",
	"net/src/ietf/datagram.ts",
	"net/src/ietf/parameters.ts",
	"net/src/ietf/publisher.ts",
	"net/src/ietf/subscriber.ts",
	"net/src/stream.ts",
	"net/src/track.ts",
];

async function sha256File(filename) {
  const hash = createHash("sha256");
  hash.update(await fs.readFile(filename));
  return hash.digest("hex");
}

async function filesBelow(directory) {
  const entries = await fs.readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    if (entry.name === "node_modules" || entry.name === ".DS_Store") continue;
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await filesBelow(filename)));
    else if (entry.isFile()) files.push(filename);
  }
  return files;
}

async function treeDigest(directory) {
  const hash = createHash("sha256");
  for (const filename of await filesBelow(directory)) {
    const relative = path.relative(directory, filename).split(path.sep).join("/");
    hash.update(relative);
    hash.update("\0");
    hash.update(await fs.readFile(filename));
    hash.update("\0");
  }
  return hash.digest("hex");
}

await fs.mkdir(path.dirname(OUTPUT), { recursive: true });

const result = await build({
  absWorkingDir: ROOT,
  alias: {
    "@moq/signals": path.join(VENDOR, "signals/src/index.ts"),
  },
  banner: {
    js: `/*! moq-dev/moq ${UPSTREAM_COMMIT}; local authorization and bounded-cache hooks; MIT OR Apache-2.0; see vendor/moq/js/LICENSE-* */`,
  },
  bundle: true,
  entryPoints: [path.join(VENDOR, "net/src/index.ts")],
  format: "esm",
  legalComments: "eof",
  metafile: true,
  nodePaths: [path.join(SCRIPT_DIR, "node_modules")],
  outfile: OUTPUT,
  platform: "browser",
  sourcemap: false,
  target: ["chrome120", "firefox153"],
});

const outputMetadata = result.metafile.outputs[path.relative(ROOT, OUTPUT)];
if (!outputMetadata) throw new Error("esbuild did not report the runtime output");
const unresolved = outputMetadata.imports.filter((entry) => entry.external);
if (unresolved.length > 0) {
  throw new Error(`bundle retains external imports: ${unresolved.map((entry) => entry.path).join(", ")}`);
}

const lock = JSON.parse(await fs.readFile(LOCK, "utf8"));
const packageVersion = (name) => lock.packages[`node_modules/${name}`]?.version;
const dependencies = {};
for (const name of Object.keys(lock.packages[""]?.dependencies ?? {}).sort()) {
  const dependency = lock.packages[`node_modules/${name}`];
  dependencies[name] = {
    version: dependency?.version ?? lock.packages[""]?.dependencies[name],
    integrity: dependency?.integrity ?? null,
  };
}
for (const name of Object.keys(lock.packages[""]?.devDependencies ?? {}).sort()) {
  const dependency = lock.packages[`node_modules/${name}`];
  dependencies[name] = {
    version: dependency?.version ?? lock.packages[""]?.devDependencies[name],
    integrity: dependency?.integrity ?? null,
  };
}

const licenseSections = [
  {
    packages: [
      "@moq/net source at 3aedaf3503146c664479f6fc53a50b36bb5e816d",
      "@moq/signals source at 3aedaf3503146c664479f6fc53a50b36bb5e816d",
      `@moq/qmux@${packageVersion("@moq/qmux")}`,
      `@moq/web-socket-stream@${packageVersion("@moq/web-socket-stream")}`,
    ],
    files: [path.join(VENDOR, "LICENSE-MIT"), path.join(VENDOR, "LICENSE-APACHE")],
  },
  {
    packages: [`@zod/mini@${packageVersion("@zod/mini")}`, `zod@${packageVersion("zod")}`],
    files: [path.join(SCRIPT_DIR, "node_modules/@zod/mini/LICENSE")],
  },
  {
    packages: [`async-mutex@${packageVersion("async-mutex")}`],
    files: [path.join(SCRIPT_DIR, "node_modules/async-mutex/LICENSE")],
  },
  {
    packages: [`bowser@${packageVersion("bowser")}`],
    files: [path.join(SCRIPT_DIR, "node_modules/bowser/LICENSE")],
  },
  {
    packages: [`tslib@${packageVersion("tslib")}`],
    files: [path.join(SCRIPT_DIR, "node_modules/tslib/LICENSE.txt")],
  },
];
let licenseBundle = "BUNDLED LICENSES FOR media-moq-library.mjs\n";
for (const section of licenseSections) {
  licenseBundle += `\n${section.packages.join("\n")}\n${"-".repeat(72)}\n`;
  for (const filename of section.files) {
    licenseBundle += `${(await fs.readFile(filename, "utf8")).trim()}\n`;
  }
}
await fs.writeFile(LICENSE_BUNDLE, licenseBundle);
await fs.appendFile(OUTPUT, `\n/*!\n${licenseBundle.replaceAll("*/", "* /")}\n*/\n`);

const manifest = {
  schema: 1,
  upstream: {
    repository: "https://github.com/moq-dev/moq",
    commit: UPSTREAM_COMMIT,
    rustPackage: "moq-net@0.2.22",
    browserSourcePackage: "@moq/net@0.3.5-source-tree-only",
    registryPackageAccepted: false,
  },
  protocol: {
    version: "draft-ietf-moq-transport-21",
    alpn: "moqt-21",
    authorizationSetupOption: "0x03",
    authorizationApplicationBytesMaximum: 8192,
  },
  build: {
    command: "cd scripts/moq-browser && npm ci && npm run build",
    node: ">=22",
    lockSha256: await sha256File(LOCK),
    scriptSha256: await sha256File(fileURLToPath(import.meta.url)),
    dependencies,
  },
  source: {
    directory: "vendor/moq/js",
    treeSha256: await treeDigest(VENDOR),
    patches: PATCHES,
    licenses: ["vendor/moq/js/LICENSE-MIT", "vendor/moq/js/LICENSE-APACHE"],
  },
  artifact: {
    path: path.relative(ROOT, OUTPUT).split(path.sep).join("/"),
    sha256: await sha256File(OUTPUT),
    bytes: (await fs.stat(OUTPUT)).size,
    externalImports: [],
    licensePath: path.relative(ROOT, LICENSE_BUNDLE).split(path.sep).join("/"),
    licenseSha256: await sha256File(LICENSE_BUNDLE),
    licenseNoticesEmbedded: true,
  },
};

await fs.writeFile(MANIFEST, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`${manifest.artifact.sha256}  ${manifest.artifact.path}`);
