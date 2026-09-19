#!/usr/bin/env node
/** Real pinned CEF load/scroll benchmark. Node >=22, no npm dependencies. */
import { spawn } from "node:child_process";
import {
  mkdtemp,
  readFile,
  writeFile,
  mkdir,
  rm,
  stat,
} from "node:fs/promises";
import {
  tmpdir,
  homedir,
  platform,
  arch,
  cpus,
  totalmem,
  loadavg,
} from "node:os";
import { join, resolve } from "node:path";
import net from "node:net";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { performance } from "node:perf_hooks";
import { once } from "node:events";
import {
  FrameParser,
  summarize,
  scrollResult,
  pacingSteps,
  collectRuns,
  validateFinalLoad,
  framePacingMetrics,
} from "./benchmark-metrics.mjs";
const scrollStepMs = 16;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const round = (x) => (typeof x === "number" ? Math.round(x * 1000) / 1000 : x);
async function until(check, timeout = 15000) {
  const end = performance.now() + timeout;
  while (performance.now() < end) {
    const value = await check();
    if (value) return value;
    await sleep(25);
  }
  throw Error("Timed out waiting for benchmark condition");
}
async function freePort() {
  const server = net.createServer();
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  await new Promise((r) => server.close(r));
  return port;
}
class CDP {
  constructor(url) {
    this.socket = new WebSocket(url);
    this.next = 0;
    this.pending = new Map();
    this.listeners = [];
    this.socket.addEventListener("message", (event) => {
      try {
        const data = JSON.parse(event.data);
        if (data.id) {
          const pending = this.pending.get(data.id);
          if (pending) {
            clearTimeout(pending.timer);
            this.pending.delete(data.id);
            data.error
              ? pending.reject(Error(JSON.stringify(data.error)))
              : pending.resolve(data.result);
          }
        } else {
          for (const listener of this.listeners)
            listener(data, performance.now());
        }
      } catch (error) {
        this.failure = error;
      }
    });
  }
  async connect() {
    await new Promise((res, rej) => {
      const timer = setTimeout(
        () => rej(Error("CDP connection timeout")),
        10000,
      );
      this.socket.addEventListener(
        "open",
        () => {
          clearTimeout(timer);
          res();
        },
        { once: true },
      );
      this.socket.addEventListener(
        "error",
        (error) => {
          clearTimeout(timer);
          rej(error);
        },
        { once: true },
      );
    });
  }
  call(method, params = {}) {
    if (this.failure) return Promise.reject(this.failure);
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(Error("CDP timeout: " + method));
      }, 15000);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const result = await this.call("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (result.exceptionDetails)
      throw Error(JSON.stringify(result.exceptionDetails));
    return result.result?.value;
  }
  close() {
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      p.reject(Error("CDP closed"));
    }
    this.pending.clear();
    this.socket.close();
  }
}
class Host {
  constructor(executable, profile, port, log) {
    this.events = [];
    this.frames = [];
    this.failure = null;
    this.child = spawn(
      executable,
      ["--profile=" + profile, "--remote-debugging-port=" + port],
      { stdio: ["pipe", "pipe", "pipe"] },
    );
    this.exit = new Promise((resolve) => {
      this.child.once("exit", resolve);
      this.child.once("error", resolve);
    });
    this.log = [];
    this.logPath = log;
    this.child.stderr.on("data", (b) => {
      if (this.log.reduce((n, x) => n + x.length, 0) < 1024 * 1024)
        this.log.push(b);
    });
    const parser = new FrameParser((packet) => {
      const target = packet.kind === "frame" ? this.frames : this.events;
      target.push(packet);
      if (target.length > 20000) target.splice(0, 1000);
    });
    this.child.stdout.on("data", (b) => {
      try {
        parser.push(b);
      } catch (e) {
        this.failure = e;
        this.child.kill();
      }
    });
    this.child.on("error", (e) => {
      this.failure = e;
      this.ended = true;
    });
    this.child.stdin.on("error", (error) => {
      this.failure = error;
    });
    this.child.on("exit", () => {
      this.ended = true;
    });
  }
  async send(value) {
    if (this.failure) throw this.failure;
    if (this.ended) throw Error("CEF host exited");
    const body = Buffer.from(JSON.stringify(value));
    const header = Buffer.alloc(5);
    header.writeUInt32LE(body.length + 1);
    header[4] = 1;
    this.child.stdin.write(header);
    if (!this.child.stdin.write(body)) await once(this.child.stdin, "drain");
  }
  async waitEvent(predicate, after = 0, timeout = 60000) {
    return until(() => {
      if (this.failure) throw this.failure;
      if (this.ended) throw Error("CEF exited");
      return this.events.find((e) => e.at >= after && predicate(e.value));
    }, timeout);
  }
  async close() {
    if (!this.ended) {
      await this.send({ op: "shutdown" }).catch(() => {});
      this.child.stdin.end();
      await Promise.race([this.exit, sleep(5000)]);
      if (!this.ended) {
        this.child.kill("SIGKILL");
        await this.exit;
      }
    }
    await writeFile(this.logPath, Buffer.concat(this.log));
  }
}
async function filterRules(directory) {
  const baseline =
    "! qOS first-run baseline\n||doubleclick.net^\n||googlesyndication.com^\n||googleadservices.com^\n||adnxs.com^\n||adsrvr.org^\n||scorecardresearch.com^\n||outbrain.com^\n||taboola.com^\n";
  const files = [
    "easylist.txt",
    "easyprivacy.txt",
    "easylist-cookie.txt",
    "ublock-filters.txt",
    "ublock-privacy.txt",
    "ublock-badware.txt",
    "ublock-resource-abuse.txt",
  ];
  let rules = baseline;
  const sources = [];
  for (const name of files) {
    try {
      const path = join(directory, name);
      if ((await stat(path)).size > 8 * 1024 * 1024) continue;
      const text = await readFile(path, "utf8");
      if (Buffer.byteLength(rules) + Buffer.byteLength(text) > 30 * 1024 * 1024)
        continue;
      rules += "\n" + text;
      sources.push({
        name,
        sha256: createHash("sha256").update(text).digest("hex"),
      });
    } catch (e) {
      if (e.code !== "ENOENT") throw e;
    }
  }
  return {
    rules,
    configuration: {
      enabled: true,
      bytes: Buffer.byteLength(rules),
      sources,
      sha256: createHash("sha256").update(rules).digest("hex"),
      customBrowserUserScripts: false,
    },
  };
}
const snapshotExpression = `(()=>{const n=performance.getEntriesByType('navigation')[0];return {url:location.href,title:document.title,readyState:document.readyState,scrollY,scrollHeight:document.scrollingElement?.scrollHeight,viewportHeight:innerHeight,viewportWidth:innerWidth,navigation:n?{domContentLoadedMs:n.domContentLoadedEventEnd,loadEventMs:n.loadEventEnd,responseStartMs:n.responseStart,transferBytes:n.transferSize}:null,firstContentfulPaintMs:performance.getEntriesByName('first-contentful-paint')[0]?.startTime??null};})()`;
async function scrollTrial(cdp, host, index) {
  await cdp.evaluate("window.__qosTrial=null;window.scrollTo(0,0)");
  await sleep(350);
  const before = await cdp.evaluate(snapshotExpression);
  if (before.scrollHeight <= before.viewportHeight + 1)
    return {
      valid: false,
      reason: "Page has no vertical scroll range",
      document: before,
    };
  const token = "sample-" + index;
  let first = null,
    last = null;
  const listener = (event, at) => {
    if (
      event.method === "Runtime.bindingCalled" &&
      event.params.name === "__qosScrollBench"
    ) {
      try {
        const payload = JSON.parse(event.params.payload);
        if (payload.token === token) {
          first ??= at;
          last = at;
        }
      } catch {}
    }
  };
  cdp.listeners.push(listener);
  try {
    await cdp.evaluate(`window.__qosTrial=${JSON.stringify(token)}`);
    const sent = performance.now();
    await host.send({ op: "scroll", x: 640, y: 400, dx: 0, dy: -120 });
    try {
      await until(() => first !== null, 1800);
    } catch {
      return {
        valid: false,
        reason: "Wheel produced no observed scrolling",
        document: before,
      };
    }
    await until(() => performance.now() - last >= 150, 3000);
    const after = await cdp.evaluate(snapshotExpression);
    return {
      ...scrollResult({
        startY: before.scrollY,
        endY: after.scrollY,
        sent,
        first,
        settled: last + 150,
      }),
      quietPeriodMs: 150,
    };
  } finally {
    cdp.listeners.splice(cdp.listeners.indexOf(listener), 1);
    await cdp.evaluate("window.__qosTrial=null").catch(() => {});
  }
}
async function pacing(cdp, host) {
  await cdp.evaluate("window.scrollTo(0,0)");
  await sleep(350);
  const before = await cdp.evaluate(snapshotExpression);
  const steps = pacingSteps(before.scrollHeight - before.viewportHeight);
  if (!steps)
    return {
      valid: false,
      reason: "Insufficient scroll range for continuous pacing",
    };
  const start = performance.now();
  let excursion = 0;
  for (let i = 0; i < steps.length; i++) {
    await host.send({ op: "scroll", x: 640, y: 400, dx: 0, dy: steps[i] });
    await sleep(scrollStepMs);
    if (i === 29)
      excursion = Math.abs((await cdp.evaluate("scrollY")) - before.scrollY);
  }
  const end = performance.now();
  const after = await cdp.evaluate(snapshotExpression);
  return {
    valid: excursion >= 1,
    excursionPixels: excursion,
    finalScrollY: after.scrollY,
    workload:
      "30 bounded downward wheel steps then 30 upward steps; avoids page-end idle gaps",
    durationMs: end - start,
    ...framePacingMetrics(host.frames, start, end),
    note: "CEF CPU frame-delivery timing, includes any other page animation; not Rust UI presentation FPS.",
  };
}
async function benchmarkSite(site, options, filters) {
  const profile = await mkdtemp(join(tmpdir(), "qos-page-bench-"));
  const port = await freePort();
  const host = new Host(
    options.host,
    profile,
    port,
    join(options.output, site.name + ".host.log"),
  );
  let cdp;
  const runs = [];
  const result = { site, runs };
  try {
    const ready = await host.waitEvent((e) => e.event === "ready", 0, 20000);
    result.runtime = ready.value;
    const targets = await until(async () => {
      try {
        const list = await (
          await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(2000) })
        ).json();
        return list.find((t) => t.type === "page");
      } catch {
        return null;
      }
    });
    cdp = new CDP(targets.webSocketDebuggerUrl);
    await cdp.connect();
    await cdp.call("Page.enable");
    await cdp.call("Runtime.enable");
    await cdp.call("Runtime.addBinding", { name: "__qosScrollBench" });
    await cdp.call("Page.addScriptToEvaluateOnNewDocument", {
      source: `if(window===window.top)addEventListener('scroll',()=>{if(window.__qosTrial)window.__qosScrollBench(JSON.stringify({token:window.__qosTrial,y:scrollY}));},{passive:true});`,
    });
    const setup = performance.now();
    await host.send({
      op: "filter_rules",
      rules: filters.rules,
      enabled: true,
    });
    await host.send({
      op: "resize",
      width: 1280,
      height: 800,
      scale_factor: 1,
    });
    await until(
      () => host.frames.some((f) => f.at >= setup && f.width === 1280),
      20000,
    );
    Object.assign(result, await collectRuns({
      count: options.runs,
      runs,
      createSample: (run) => ({
        run: run + 1,
        cache: run === 0 ? "fresh isolated profile" : "warm same profile",
        requestedUrl: site.url,
        systemLoadAverage: loadavg(),
      }),
      reset: async () => {
        const blankStart = performance.now();
        await host.send({ op: "navigate", url: "about:blank" });
        await host.waitEvent(
          (e) => e.event === "loaded" && e.url === "about:blank",
          blankStart,
        );
      },
      measure: async (sample, run) => {
        const start = performance.now();
        await host.send({ op: "navigate", url: site.url });
        const loaded = await host.waitEvent(
          (e) =>
            (e.event === "loaded" && e.url !== "about:blank") ||
            e.event === "error",
          start,
          60000,
        );
        sample.requestToLoadEventMs = loaded.at - start;
        sample.httpStatus = loaded.value.status ?? null;
        if (
          loaded.value.event === "error" ||
          !(sample.httpStatus >= 200 && sample.httpStatus < 400)
        )
          throw Error(
            "Unsuccessful document load: " + JSON.stringify(loaded.value),
          );
        await sleep(1500);
        sample.document = await until(async () => {
          const document = await cdp.evaluate(snapshotExpression);
          return document.readyState === "complete" &&
            document.navigation?.loadEventMs > 0 &&
            document.title
            ? document
            : null;
        }, 30000);
        validateFinalLoad(sample, host.events, start);
        const expected = new URL(site.url),
          actual = new URL(sample.document.url);
        if (actual.hostname !== expected.hostname)
          throw Error(
            "Redirected to another host (possible consent/interstitial): " +
              actual.hostname,
          );
        if (!sample.document.title || sample.document.readyState !== "complete")
          throw Error("Document not ready");
        sample.loadValid = true;
        const shot = await cdp.call("Page.captureScreenshot", {
          format: "png",
          captureBeyondViewport: false,
        });
        await writeFile(
          join(options.output, `${site.name}-${run + 1}.png`),
          Buffer.from(shot.data, "base64"),
        );
        sample.scroll = [];
        for (let trial = 0; trial < 5; trial++) {
          const result = await scrollTrial(cdp, host, `${run}-${trial}`);
          sample.scroll.push(result);
          if (!result.valid) break;
        }
        sample.scrollResponseMs = summarize(
          sample.scroll.filter((s) => s.valid).map((s) => s.responseMs),
        );
        sample.scrollSettleMs = summarize(
          sample.scroll.filter((s) => s.valid).map((s) => s.settleMs),
        );
        sample.scrollPacing = sample.scrollResponseMs
          ? await pacing(cdp, host)
          : { valid: false, reason: "No valid scroll samples" };
      },
      onSample: (sample) => console.log(JSON.stringify({ site: site.name, ...sample })),
    }));
  } catch (error) {
    result.error = String(error);
  } finally {
    // Cleanup errors must not discard already measured samples.
    for (const cleanup of [() => cdp?.close(), () => host.close(), () => rm(profile, { recursive: true, force: true })]) {
      try { await cleanup(); }
      catch (error) { result.error = [result.error, `Cleanup failed: ${error}`].filter(Boolean).join("; "); }
    }
  }
  return result;
}
export function markdown(report) {
  const lines = [
    "# Browser page benchmark",
    "",
    `Recorded ${report.recordedAt}; ${report.environment.platform} ${report.environment.arch}; 1280×800.`,
    "",
    "**Scope:** pinned CEF with built-in native blocking and a cached filter-list snapshot. CPU frame delivery excludes Rust/Vello presentation. Extra browser user scripts are not injected. First run uses a fresh isolated profile; subsequent runs use its warm cache.",
    "",
    "| Site | Run | Load event (ms) | FCP (ms) | Scroll response median (ms) | Scroll settle median (ms) | Frame interval p95 (ms) |",
    "|---|---:|---:|---:|---:|---:|---:|",
  ];
  for (const result of report.sites) {
    if (result.error)
      lines.push(`| ${result.site.name} | failed | ${String(result.error).replaceAll("|", "\\|").replaceAll("\n", " ")} | n/a | n/a | n/a | n/a |`);
    for (const s of result.runs)
      lines.push(
        `| ${result.site.name} | ${s.run} (${s.cache.startsWith("fresh") ? "fresh" : "warm"}) | ${s.loadValid ? round(s.requestToLoadEventMs) : "invalid"} | ${round(s.document?.firstContentfulPaintMs) ?? "n/a"} | ${round(s.scrollResponseMs?.median) ?? "n/a"} | ${round(s.scrollSettleMs?.median) ?? "n/a"} | ${round(s.scrollPacing?.frameIntervalsMs?.p95) ?? "n/a"} |`,
      );
  }
  lines.push(
    "",
    "Scroll response includes loopback CDP observation latency. Settle includes a 150 ms quiet period. Invalid/non-scrolling pages are not zero-time successes. Frame pacing may include video/other page animation. Small sample sizes do not establish general browser performance. See JSON for raw samples, errors, URLs, titles, runtime and filter hashes.",
  );
  return lines.join("\n") + "\n";
}
async function main() {
  const args = process.argv.slice(2);
  const value = (name, fallback) => {
    const i = args.indexOf(name);
    if (i < 0) return fallback;
    if (!args[i + 1]) throw Error("Missing " + name);
    return args[i + 1];
  };
  const options = {
    host: resolve(
      value(
        "--host",
        "target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host",
      ),
    ),
    output: resolve(value("--output", "target/browser-page-benchmark")),
    runs: Number(value("--runs", "3")),
  };
  if (!Number.isInteger(options.runs) || options.runs < 1 || options.runs > 5)
    throw Error("--runs must be 1..5");
  await mkdir(options.output, { recursive: true });
  try {
    await stat(join(options.output, "results.json"));
    throw Error("Output already has results; choose a new directory");
  } catch (e) {
    if (e.code !== "ENOENT") throw e;
  }
  const cache = value(
    "--filter-cache",
    platform() === "darwin"
      ? join(homedir(), "Library/Caches/qos-browser/filters")
      : join(
          process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"),
          "qos-browser/filters",
        ),
  );
  const filters = await filterRules(cache);
  const siteFile = value("--sites", null);
  const sites = siteFile
    ? JSON.parse(await readFile(siteFile, "utf8"))
    : [
        { name: "wikipedia", url: "https://en.wikipedia.org/wiki/Main_Page" },
        { name: "hackernews", url: "https://news.ycombinator.com/" },
        {
          name: "youtube",
          url: "https://www.youtube.com/results?search_query=nature",
        },
      ];
  if (!Array.isArray(sites) || !sites.length || sites.length > 10)
    throw Error("Expected 1..10 sites");
  for (const site of sites) {
    const url = new URL(site.url);
    if (
      !/^[a-z0-9_-]{1,40}$/.test(site.name) ||
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password
    )
      throw Error("Invalid benchmark site");
  }
  const report = {
    recordedAt: new Date().toISOString(),
    environment: {
      platform: platform(),
      arch: arch(),
      cpu: cpus()[0]?.model,
      memoryBytes: totalmem(),
      node: process.version,
      logicalCpus: cpus().length,
      systemLoadAverage: loadavg(),
      concurrentDevelopmentWork:
        "Other development builds may run; load averages are recorded per sample.",
    },
    configuration: {
      host: options.host,
      hostSha256: createHash("sha256")
        .update(await readFile(options.host))
        .digest("hex"),
      harnessSha256: createHash("sha256")
        .update(await readFile(new URL(import.meta.url)))
        .digest("hex"),
      viewport: [1280, 800],
      runs: options.runs,
      blocking: filters.configuration,
      scrollTrials: 5,
      continuousScrollStepMs: scrollStepMs,
      wheelDelta: -120,
      scrollSettleQuietMs: 150,
      sandbox: "unchanged/enabled",
      tls: "unchanged/enabled",
      scope: "Native CEF engine/CPU delivery; excludes Rust UI presentation",
    },
    sites: [],
  };
  for (const site of sites) {
    try {
      report.sites.push(await benchmarkSite(site, options, filters));
    } catch (error) {
      report.sites.push({ site, runs: [], error: String(error) });
    }
    await writeFile(
      join(options.output, "results.json"),
      JSON.stringify(report, null, 2) + "\n",
    );
    await writeFile(join(options.output, "results.md"), markdown(report));
  }
  console.log("Saved " + join(options.output, "results.md"));
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
