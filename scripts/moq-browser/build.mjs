// Bundle the published @moq/net (pinned in package.json / bun.lock) into the
// browser runtime's self-contained `media-moq-library.mjs`, with the bundled
// packages' licenses next to it. Nothing here is committed: the browser
// runtime build (crates/browser/tilo-browser-runtime/scripts/build.sh) runs
//
//   bun install --frozen-lockfile && bun build.mjs <output.mjs>
//
// and the web tests run it once with no argument, which writes into
// crates/browser/tilo-browser-runtime/web/ (ignored by git).
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

const SCRIPT_DIR = import.meta.dir;
const ROOT = path.resolve(SCRIPT_DIR, "../..");
const output = path.resolve(
  process.argv[2] ?? path.join(ROOT, "crates/browser/tilo-browser-runtime/web/media-moq-library.mjs"),
);
const licenseOutput = output.replace(/\.mjs$/, ".LICENSE.txt");

const result = await Bun.build({
  entrypoints: [path.join(SCRIPT_DIR, "entry.mjs")],
  target: "browser",
  format: "esm",
  minify: false,
  sourcemap: "none",
  throw: false,
});
if (!result.success) {
  for (const message of result.logs) console.error(message);
  process.exit(1);
}
const [artifact] = result.outputs;
const source = await artifact.text();
// A self-contained module: no bare imports may remain for the page to resolve.
const bare = [...source.matchAll(/^\s*(?:import|export)\s[^;]*?from\s*["']([^"'./][^"']*)["']/gm)].map((m) => m[1]);
if (bare.length > 0) throw new Error(`bundle retains external imports: ${bare.join(", ")}`);

const lock = await readFile(path.join(SCRIPT_DIR, "bun.lock"), "utf8");
const packages = [...lock.matchAll(/^\s*"([^"]+)": \["([^"]+@[^"]+)"/gm)].map((m) => ({ name: m[1], id: m[2] }));
let licenses = "BUNDLED LICENSES FOR media-moq-library.mjs\n";
for (const { name, id } of packages) {
  const directory = path.join(SCRIPT_DIR, "node_modules", name);
  let text;
  for (const file of ["LICENSE", "LICENSE.md", "LICENSE.txt", "LICENSE-MIT"]) {
    try {
      text = await readFile(path.join(directory, file), "utf8");
      break;
    } catch {}
  }
  if (text === undefined) {
    const manifest = JSON.parse(await readFile(path.join(directory, "package.json"), "utf8"));
    // The @moq packages ship without license files; their upstream texts
    // (github.com/moq-dev/moq) are kept in licenses/ and appended once below.
    text = `License: ${manifest.license}${name.startsWith("@moq/") ? " (texts: moq-dev/moq, below)" : ""}`;
  }
  licenses += `\n${id}\n${"-".repeat(72)}\n${text.trim()}\n`;
}
for (const file of ["moq-dev-LICENSE-MIT", "moq-dev-LICENSE-APACHE"]) {
  const text = await readFile(path.join(SCRIPT_DIR, "licenses", file), "utf8");
  licenses += `\n${file} (github.com/moq-dev/moq)\n${"-".repeat(72)}\n${text.trim()}\n`;
}

await mkdir(path.dirname(output), { recursive: true });
await writeFile(licenseOutput, licenses);
await writeFile(
  output,
  `/*! @moq/net and its dependencies, bundled from the published npm packages; licenses in ${path.basename(licenseOutput)} */\n${source}`,
);
console.log(output);
