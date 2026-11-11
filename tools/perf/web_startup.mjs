// Startup of the web shell, measured in a real browser.
//
// The page already marks its startup phases (`tilo:module`, `tilo:compiled`,
// `tilo:runtime`, `tilo:peer`, `tilo:wasm-ready`,
// `tilo:storage`, `tilo:assets`, `tilo:mounted`, `tilo:first-frame`), all on the
// navigation timeline, so a slow phase names itself instead of hiding in
// "loading". This runner serves a built bundle, opens a page, waits for the
// first presented frame and reports the marks plus the wasm transfer.
//
//   node tools/perf/web_startup.mjs --bundle DIR [--device CLASS] [--page index.html]
//                                   [--json PATH] [--timeout MS]
//
// `--device` labels the machine the way `tools/perf/kpi-budgets.toml` does
// (`web-desktop`, `web-opi`, ...); it defaults to `$TILO_PERF_DEVICE`.
import {launch, serve, sleep} from '../../tools/web/chrome.mjs';
import {writeFileSync} from 'node:fs';
import {join} from 'node:path';

const PHASES = ['module', 'compiled', 'runtime', 'peer', 'wasm-ready', 'storage', 'assets', 'mounted', 'first-frame'];

/** Records the interval between presented frames until the page is read. */
const FRAME_RECORDER = () => {
  window.__tiloFrames = [];
  let last = 0;
  const tick = now => {
    if (last) window.__tiloFrames.push(now - last);
    last = now;
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
};

/** `p`th percentile of `values`, nearest rank, and their count and maximum. */
function summary(values) {
  if (!values.length) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const at = p => sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
  return {
    count: sorted.length,
    p50_ms: at(0.5),
    p95_ms: at(0.95),
    max_ms: sorted[sorted.length - 1],
  };
}

function arguments_(argv) {
  const options = {page: 'index.html', timeout: 120000};
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--help' || flag === '-h') {
      console.log(`web_startup --bundle DIR [--device CLASS] [--page PAGE] [--json PATH] [--timeout MS] [--warm]

Measures a page load from navigation start to the first presented desktop
frame: the page's own startup marks, the wasm module's transfer and compile,
the JavaScript heap after the first frame, and the frames it presents while
the strip pans. --warm loads the page a second time in the same browser, as a
person coming back to it: the module is in the browser's cache then.`);
      process.exit(0);
    }
    if (flag === '--warm') {
      options.warm = true;
      continue;
    }
    // `--flag=value` for values that themselves start with a dash, as a
    // browser switch does.
    if (flag.startsWith('--chrome-arg=')) {
      (options.chromeArgs ??= []).push(flag.slice('--chrome-arg='.length));
      continue;
    }
    const value = argv[i + 1];
    if (value === undefined) throw new Error(`${flag} needs a value`);
    i += 1;
    if (flag === '--bundle') options.bundle = value;
    else if (flag === '--device') options.device = value;
    else if (flag === '--page') options.page = value;
    else if (flag === '--json') options.json = value;
    else if (flag === '--timeout') options.timeout = Number(value);
    else if (flag === '--chrome-arg') (options.chromeArgs ??= []).push(value);
    else throw new Error(`unknown argument ${flag}`);
  }
  options.device ??= process.env.TILO_PERF_DEVICE ?? 'web-desktop';
  if (!options.bundle) throw new Error('--bundle is required (a directory build.sh wrote)');
  return options;
}

/** Graphics start-up's step, as the page names it (`data-gpu-step`), if it says. */
const readGpuStep = () => document.documentElement.dataset.gpuStep;

/** The page's marks, by name, in milliseconds from navigation start. */
const readMarks = () => {
  const marks = {};
  for (const entry of performance.getEntriesByType('mark')) {
    const name = entry.name.startsWith('tilo:') ? entry.name.slice('tilo:'.length) : null;
    if (name) marks[name] = entry.startTime;
  }
  const wasm = performance
    .getEntriesByType('resource')
    .find(entry => entry.name.endsWith('.wasm'));
  return {
    marks,
    wasm: wasm && {
      duration_ms: wasm.duration,
      transfer_bytes: wasm.transferSize,
      decoded_bytes: wasm.decodedBodySize,
    },
    heap_mib: performance.memory ? performance.memory.usedJSHeapSize / 1048576 : null,
    // Everything the page fetched until now, as the network carried it:
    // what a person waits on before the first frame, whatever kind it is.
    transfer: (() => {
      const resources = performance.getEntriesByType('resource');
      const kind = name => {
        const path = new URL(name).pathname;
        if (path.endsWith('.wasm')) return path.includes('/pkg/tilo_browser_runtime_bg') ? 'start_module' : 'split_modules';
        if (/\.(woff2?|ttf|otf)$/.test(path) || path.includes('/fonts/')) return 'fonts';
        if (/\.m?js$/.test(path)) return 'scripts';
        return 'other';
      };
      const total = {bytes: 0, count: resources.length};
      const byKind = {};
      for (const entry of resources) {
        total.bytes += entry.transferSize;
        byKind[kind(entry.name)] = (byKind[kind(entry.name)] ?? 0) + entry.transferSize;
      }
      return {total_bytes: total.bytes, requests: total.count, by_kind: byKind};
    })(),
  };
};

const main = async () => {
  const options = arguments_(process.argv.slice(2));
  const served = await serve(options.bundle);
  const browser = await launch({args: options.chromeArgs ?? []});
  try {
    const tab = await browser.tab();
    // The desktop page is the bundle's root: everything it links lives after
    // the `#`, so any other path is a page it refuses to open.
    const at = options.page === 'index.html' ? `${served.url}/` : `${served.url}/${options.page}`;
    const navigation = await tab.navigate(at);
    if (navigation.errorText) throw new Error(navigation.errorText);
    const deadline = Date.now() + options.timeout;
    let present;
    // Graphics start-up names the step it waits on: device, pipelines,
    // surface. A slow first frame that sits on one of them is the browser's
    // rasteriser, not the page's own work, and only the sampled steps say
    // which. Sampled from navigation, so they read in the same clock as the
    // page's marks.
    const steps = [];
    const stepped = async step => {
      if (step && steps.at(-1)?.step !== step) {
        steps.push({step, at_ms: await tab.evaluate('performance.now()').catch(() => null)});
      }
    };
    for (;;) {
      await stepped(await tab.evaluate(`(${readGpuStep.toString()})()`).catch(() => null));
      present = await tab.evaluate(`(${readMarks.toString()})()`).catch(() => null);
      if (present?.marks['first-frame'] !== undefined) break;
      const failed = await tab
        .evaluate(`document.body?.innerText?.slice(0, 400) ?? ''`)
        .catch(() => '');
      if (Date.now() > deadline) {
        throw new Error(
          `no first frame within ${options.timeout} ms${failed ? `; page says: ${failed}` : ''}\n${tab.log.join('\n')}`,
        );
      }
      if (tab.log.some(line => line.startsWith('EXC '))) break;
      await sleep(100);
    }
    if (present?.marks['first-frame'] === undefined) {
      // The page failed before it drew: its own words say why, and they are
      // worth more than a timeout.
      const said = await tab
        .evaluate(`document.body?.innerText?.slice(0, 400) ?? ''`)
        .catch(() => '');
      throw new Error(
        `no first frame${said ? `; page says: ${said}` : ''}\n${tab.log.join('\n')}`,
      );
    }
    // The frame after the first is what a person sees as "it answers": wait
    // for one more presented state, then read the settled numbers.
    const settled = await tab
      .waitFor(`(${readMarks.toString()})().marks['first-frame'] !== undefined && performance.getEntriesByType('mark').length > 6`, {
        timeout: Math.max(1000, options.timeout - (Date.now() - (deadline - options.timeout))),
      })
      .then(() => tab.evaluate(`(${readMarks.toString()})()`))
      .catch(() => present);
    const marks = settled.marks;
    const firstFrame = marks['first-frame'];
    // Frames the page presented while the strip pans: the animation a person
    // feels first, and the only place the page's own frame cost shows
    // (headless Chrome has no compositor to blame for a slow one).
    const frames = await tab
      .evaluate(`(${FRAME_RECORDER.toString()})()`)
      .then(async () => {
        const size = await tab.evaluate('({w: innerWidth, h: innerHeight})');
        const x = Math.round(size.w / 2);
        const y = Math.round(size.h / 2);
        for (let step = 0; step < 6; step += 1) {
          await tab.send('Input.dispatchMouseEvent', {
            type: 'mouseWheel',
            x,
            y,
            deltaX: 160,
            deltaY: 0,
          });
          await sleep(80);
        }
        await sleep(900);
        return tab.evaluate('window.__tiloFrames ?? []');
      })
      .then(summary)
      .catch(() => null);
    // What the page went on to fetch once it drew (the modules it loads in
    // its idle time, the apps it prefetches), a few seconds on.
    const idle = await sleep(4000)
      .then(() => tab.evaluate(`(${readMarks.toString()})()`))
      .then(read => read.transfer)
      .catch(() => null);
    // The same page again in the same browser: what a person coming back to
    // it gets, with the module already fetched and compiled once.
    const warm = options.warm
      ? await tab
          .navigate(at)
          .then(() =>
            tab.waitFor(
              `performance.getEntriesByType('mark').some(m => m.name === 'tilo:first-frame')`,
              {timeout: options.timeout},
            ),
          )
          .then(() => tab.evaluate(`(${readMarks.toString()})()`))
          .then(read =>
            read.marks['first-frame'] === undefined
              ? null
              : {
                  first_frame_ms: read.marks['first-frame'],
                  marks_ms: read.marks,
                  wasm: read.wasm ?? null,
                },
          )
          .catch(() => null)
      : null;
    const gaps = {};
    let previous = 0;
    for (const phase of PHASES) {
      if (marks[phase] === undefined) continue;
      gaps[phase] = marks[phase] - previous;
      previous = marks[phase];
    }
    const measurement = {
      kind: 'web_startup',
      device: options.device,
      page: options.page,
      first_frame_ms: firstFrame,
      marks_ms: marks,
      gaps_ms: gaps,
      gpu_steps: steps,
      wasm: settled.wasm ?? null,
      heap_mib: settled.heap_mib ?? null,
      transfer_at_first_frame: present.transfer ?? null,
      transfer_after_idle: idle,
      frames: frames ?? null,
      warm: warm ?? null,
      errors: tab.log.filter(line => line.startsWith('EXC ')),
    };
    console.log(`device ${measurement.device}  page ${measurement.page}`);
    console.log(`first frame      ${firstFrame.toFixed(1)} ms`);
    if (measurement.warm) {
      console.log(`warm first frame ${measurement.warm.first_frame_ms.toFixed(1)} ms`);
    }
    for (const [phase, ms] of Object.entries(gaps)) {
      console.log(`  ${phase.padEnd(14)} ${ms.toFixed(1)} ms`);
    }
    if (measurement.gpu_steps.length) {
      console.log(
        `graphics         ${measurement.gpu_steps
          .map(({step, at_ms}) => `${step} @ ${at_ms?.toFixed(0) ?? '?'} ms`)
          .join(', ')}`,
      );
    }
    for (const [when, transfer] of [['at first frame', measurement.transfer_at_first_frame], ['after idle', measurement.transfer_after_idle]]) {
      if (transfer) {
        console.log(`transfer ${when.padEnd(15)} ${(transfer.total_bytes / 1048576).toFixed(2)} MiB in ${transfer.requests} requests ` +
          Object.entries(transfer.by_kind).map(([kind, bytes]) => `${kind} ${(bytes / 1048576).toFixed(2)}`).join(', '));
      }
    }
    if (measurement.wasm) {
      console.log(
        `wasm             ${(measurement.wasm.transfer_bytes / 1048576).toFixed(1)} MiB transferred, ` +
          `${(measurement.wasm.duration_ms / 1000).toFixed(2)} s`,
      );
    }
    if (measurement.heap_mib !== null) {
      console.log(`heap             ${measurement.heap_mib.toFixed(0)} MiB`);
    }
    if (measurement.frames) {
      const {count, p50_ms, p95_ms, max_ms} = measurement.frames;
      console.log(
        `frames           ${count} over a pan: p50 ${p50_ms.toFixed(1)} ms, ` +
          `p95 ${p95_ms.toFixed(1)} ms, max ${max_ms.toFixed(1)} ms`,
      );
    }
    if (measurement.errors.length) console.log(measurement.errors.join('\n'));
    if (options.json) {
      writeFileSync(options.json, JSON.stringify(measurement, null, 2));
      console.log(`wrote ${options.json}`);
    }
    await tab.close();
    return measurement.errors.length ? 1 : 0;
  } finally {
    await browser.close();
    await served.close();
  }
};

main().then(
  code => process.exit(code),
  error => {
    console.error(error.message ?? String(error));
    process.exit(2);
  },
);
