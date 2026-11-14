// Record DOMSnapshot fixtures for the converter's tests from a real engine.
//
//   node crates/browser/paint-stream/tools/record-fixture.mjs [chrome-binary] [page]
//
// Loads each page of tests/fixtures (`layers.html`, `form.html`) at 800x600
// and saves, per state, the raw `DOMSnapshot.captureSnapshot` result
// (`<page>.<state>.json`), and for `layers.top` the engine's own pixels
// (`layers.top.png`). `page` records that one page only, leaving the other's
// fixtures as they are; an empty `chrome-binary` is the default one. The
// computed-style list is read from src/snapshot.rs so the fixture always
// matches what the host requests.
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync, rmSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "..", "tests", "fixtures");
const chromeBin = process.argv[2] || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const only = process.argv[3];
const src = readFileSync(join(here, "..", "src", "snapshot.rs"), "utf8");
// The list as it is declared, not where the module's own text mentions it.
const declared = src.indexOf("pub const COMPUTED_STYLES");
const block = src.slice(declared, src.indexOf("];", declared));
const computedStyles = [...block.matchAll(/"([a-z-]+)"/g)].map((m) => m[1]);
if (declared < 0 || computedStyles.length === 0) throw new Error("src/snapshot.rs declares no COMPUTED_STYLES to record with");
const PORT = 9334;

const profile = mkdtempSync(join(tmpdir(), "paint-fixture-"));
const chrome = spawn(chromeBin, [
  "--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${profile}`,
  "--hide-scrollbars", "--no-first-run", "--font-render-hinting=none", "about:blank",
], { stdio: "ignore" });
const exited = new Promise((r) => chrome.once("exit", r));

async function target() {
  for (let i = 0; i < 100; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      const page = list.find((t) => t.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("no page target");
}

const ws = new WebSocket(await target());
await new Promise((r) => (ws.onopen = r));
let id = 0;
const pending = new Map();
const loads = [];
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) {
    const { res, rej } = pending.get(msg.id);
    pending.delete(msg.id);
    msg.error ? rej(new Error(JSON.stringify(msg.error))) : res(msg.result);
  } else if (msg.method === "Page.loadEventFired") {
    loads.splice(0).forEach((r) => r());
  }
};
const send = (method, params = {}) => new Promise((res, rej) => {
  pending.set(++id, { res, rej });
  ws.send(JSON.stringify({ id, method, params }));
});

try {
  await send("Page.enable");
  await send("DOMSnapshot.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 800, height: 600, deviceScaleFactor: 1, mobile: false });
  const pages = {
    layers: {
      top: "window.scrollTo({ top: 0, behavior: 'instant' })",
      scrolled: "window.scrollTo({ top: 300, behavior: 'instant' })",
      sidebar: "document.getElementById('side').scrollTop = 100",
      typed: "document.getElementById('q').value = 'hello'",
      password: "document.getElementById('pw').value = 'secret'",
    },
    // A form as it loads, and with every kind of answer given: text, a
    // ticked box, one of two, an option chosen and lines of an address.
    form: {
      empty: "0",
      filled: `document.getElementById('given').value = 'Maria Elena';
        document.getElementById('agree').checked = true;
        document.getElementById('refused-no').checked = true;
        document.getElementById('purpose').value = 'Visiting family';
        document.getElementById('address').value = '14 Canal Street, Flat 3\\nUtrecht 3511 AB';`,
    },
  };
  for (const [page, states] of Object.entries(pages)) {
    if (only && only !== page) continue;
    const loaded = new Promise((r) => loads.push(r));
    await send("Page.navigate", { url: pathToFileURL(join(fixtures, `${page}.html`)).href });
    await loaded;
    for (const [name, script] of Object.entries(states)) {
      await send("Runtime.evaluate", { expression: script });
      await new Promise((r) => setTimeout(r, 200));
      const snapshot = await send("DOMSnapshot.captureSnapshot", {
        computedStyles, includePaintOrder: true, includeDOMRects: true,
      });
      // The page's own URL names this checkout; fixtures carry a placeholder.
      const json = JSON.stringify(snapshot).replaceAll(pathToFileURL(fixtures).href, "file:///fixtures");
      writeFileSync(join(fixtures, `${page}.${name}.json`), json);
      // The renderer's fidelity test compares against the first state only.
      if (page === "layers" && name === "top") {
        const shot = await send("Page.captureScreenshot", { format: "png" });
        writeFileSync(join(fixtures, `layers.${name}.png`), Buffer.from(shot.data, "base64"));
      }
      console.log(`recorded ${page}.${name}`);
    }
  }
} finally {
  ws.close();
  chrome.kill("SIGTERM");
  await exited;
  rmSync(profile, { recursive: true, force: true, maxRetries: 5 });
}
