// A user story as it is walked: each person a browser of their own (a fresh
// profile, so a fresh root and device), recorded as a video of everything their
// page showed, and each step a plain-words name, a budget, a screenshot of every
// person's page as it ended and how long it took. A step that fails or runs
// over its budget keeps the screenshot, the pages' console and errors, and what
// the stack's services logged while it ran.
//
//   const story = recorder({id, out, logs: [server.log, …]});
//   const ada = await story.person('Ada');
//   await story.step('Ada opens Tilo', async () => { … });
//   await story.finish();          // story.json, timings.json, the videos
//
// Each step's budget is budgets.json's, by the step's words, so a budget is
// tightened there and nowhere else. A step that throws stops the story there;
// one that only ran over its budget is a failure too, and the story goes on, so
// the steps after it are still seen.
//
// What the web page costs a person is measured the same way in every story
// (`story.web`): its cold and warm load to first content, signing in to a
// workspace, opening an app, an action's round trip, how typing feels (key to
// paint) and the script it sends. Each is held to budgets.json's `web` target,
// and one over it makes the story red as a step over budget does.
import {spawnSync} from 'node:child_process';
import {closeSync, copyFileSync, existsSync, mkdirSync, openSync, readFileSync, readSync, rmSync, statSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {shellMetrics} from '../integration/waits.mjs';
import {recordLoading, returning} from '../integration/shell.mjs';

/** What stopped a story: the step's own failure, already recorded. */
export class StoryStopped extends Error {}

/** Every budget the stories are held to (budgets.json). */
/** A time as a person reads it: milliseconds under a second, else seconds. */
const duration = ms => ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;

export const BUDGETS = JSON.parse(readFileSync(new URL('./budgets.json', import.meta.url), 'utf8'));

/** The `p`th percentile (0-100) of `values`, nearest rank. */
const percentile = (values, p) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1))];
};

// In the page: from each key to the first frame presented after it. The
// desktop drawn into a canvas says when it presented a frame
// (`data-frames` on the canvas's host); a page of plain HTML paints on the
// browser's own next frame, which is the animation frame's turn and the task
// after it.
const KEY_TO_PAINT = `(() => {
  if (globalThis.__keyPaint) return true;
  globalThis.__keyPaint = [];
  addEventListener('keydown', event => {
    if (event.key.length !== 1) return;
    const pressed = performance.now();
    const done = () => globalThis.__keyPaint.push(performance.now() - pressed);
    const host = globalThis.tiloMount?.canvas?.parentElement;
    if (host && host.dataset.frames !== undefined) {
      const from = Number(host.dataset.frames);
      const watch = new MutationObserver(() => {
        if (Number(host.dataset.frames) > from && host.dataset.phase === 'presented') { watch.disconnect(); done(); }
      });
      watch.observe(host, {attributes: true, attributeFilter: ['data-frames', 'data-phase']});
    } else {
      requestAnimationFrame(() => { const channel = new MessageChannel(); channel.port1.onmessage = done; channel.port2.postMessage(0); });
    }
  }, true);
  return true;
})()`;
// The page's first content, as the browser timed it from the navigation.
const FIRST_CONTENT = `(performance.getEntriesByName('first-contentful-paint')[0]?.startTime ?? 0)`;

const slug = text => text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 60);
// What a service's log line says without its colours.
const plain = text => text.replace(/\x1b\[[0-9;]*m/g, '');
// What a page said that a person would call broken: exceptions, crashes,
// errors, and requests that failed. The rest of the console is kept too, but
// these are what a failing step names.
const complaint = line => /^(EXC|CRASH|ERROR|NET|HTTP [45])/.test(line) || /panic/i.test(line);

export function recorder({id, out, logs = []}) {
  mkdirSync(join(out, 'shots'), {recursive: true});
  mkdirSync(join(out, 'evidence'), {recursive: true});
  const began = Date.now();
  const steps = [];
  const timings = {};
  const people = [];
  const measures = [];
  let stopped = null;
  const budgetOf = title => {
    const budget = BUDGETS.steps?.[id]?.[title];
    if (!Number.isFinite(budget)) throw new Error(`budgets.json gives the ${id} step "${title}" no budget`);
    return budget;
  };

  /** Where each service's log ends now, so a step reads what it logged. */
  const logMarks = () => Object.fromEntries(logs.map(file => [file, existsSync(file) ? statSync(file).size : 0]));
  /** What each service logged since `marks`: its last 120 lines each. */
  const loggedSince = marks => logs.map(file => {
    if (!existsSync(file)) return '';
    const from = marks[file] ?? 0;
    const size = statSync(file).size;
    if (size <= from) return '';
    const length = Math.min(size - from, 4 << 20);
    const buffer = Buffer.alloc(length);
    const fd = openSync(file, 'r');
    try { readSync(fd, buffer, 0, length, size - length); } finally { closeSync(fd); }
    const lines = plain(buffer.toString('utf8')).split('\n').filter(Boolean).slice(-120);
    return `=== ${file.split('/').slice(-1)[0]} (${lines.length} lines)\n${lines.join('\n')}`;
  }).filter(Boolean).join('\n\n');

  /** A person: a browser of their own with the shell's window, its page
   * recorded frame by frame as the browser draws it. */
  const person = async name => {
    // A synthetic microphone and no prompt for it, as people.mjs's people get:
    // the people here can be called.
    const launched = await launch({media: true});
    const tab = await launched.tab();
    await tab.send('Emulation.setDeviceMetricsOverride', shellMetrics).catch(() => {});
    await returning(tab);
    await recordLoading(launched, tab);
    const frames = join(out, 'frames', slug(name));
    mkdirSync(frames, {recursive: true});
    const one = {name, tab, launched, frames, shots: [], kept: [], bytes: {script: 0, wasm: 0}};
    // What the page's code costs to fetch: compressed bytes on the wire, by
    // kind, from the browser's own account of each response.
    const kinds = new Map();
    launched.on(({sessionId, method, params}) => {
      if (sessionId !== tab.sessionId) return;
      if (method === 'Network.responseReceived') {
        const mime = params.response.mimeType ?? '';
        const kind = params.type === 'Script' || /javascript/.test(mime) ? 'script' : /wasm/.test(mime) ? 'wasm' : null;
        if (kind) kinds.set(params.requestId, kind);
      } else if (method === 'Network.loadingFinished' && kinds.has(params.requestId)) {
        one.bytes[kinds.get(params.requestId)] += params.encodedDataLength;
        kinds.delete(params.requestId);
      }
    });
    // Every frame the page presents, as the browser hands it over: each kept
    // with the time it was drawn, so the video plays at the pace the person saw.
    launched.on(message => {
      if (message.sessionId !== tab.sessionId || message.method !== 'Page.screencastFrame') return;
      const {data, metadata, sessionId: frame} = message.params;
      const file = `${String(one.kept.length).padStart(5, '0')}.jpg`;
      writeFileSync(join(frames, file), Buffer.from(data, 'base64'));
      one.kept.push({file, at: metadata.timestamp});
      tab.send('Page.screencastFrameAck', {sessionId: frame}).catch(() => {});
    });
    await tab.send('Page.startScreencast', {format: 'jpeg', quality: 60, maxWidth: shellMetrics.width, maxHeight: shellMetrics.height});
    people.push(one);
    return one;
  };

  // A step's picture: a screenshot, or, when the page gives none at once (it
  // is between documents, and a capture waits for a frame of the one it left),
  // the last frame the browser drew for the screencast, which is what the
  // person was looking at. The story never waits on a picture.
  const shoot = async (one, file) => {
    let timer;
    const late = new Promise(resolve => { timer = setTimeout(() => resolve(null), 3000); });
    const shot = await Promise.race([one.tab.screenshot().catch(() => null), late]).finally(() => clearTimeout(timer));
    if (shot) {
      writeFileSync(join(out, 'shots', file), shot);
      return `shots/${file}`;
    }
    const last = one.kept.at(-1);
    if (!last) return null;
    const drawn = file.replace(/\.png$/, '.jpg');
    copyFileSync(join(one.frames, last.file), join(out, 'shots', drawn));
    return `shots/${drawn}`;
  };

  /** One step of the story: `run` does what the person does and waits for
   * what they would wait for. It may take `budget` ms; past that the step
   * is a failure, and past `limit` (default: four budgets, at least a
   * minute and a half, so a wait inside the step that runs out first says
   * what it waited for) it is stopped as hung. */
  const step = async (title, run, {limit, budgetKey = title} = {}) => {
    if (stopped) return undefined;
    const budget = budgetOf(budgetKey);
    limit ??= Math.max(4 * budget, 90_000);
    const index = steps.length + 1;
    const record = {index, title, budgetMs: budget, atMs: Date.now() - began};
    steps.push(record);
    const marks = logMarks();
    const consoles = people.map(one => [one, one.tab.log.length]);
    const from = Date.now();
    let value, error = null, timer;
    try {
      value = await Promise.race([
        run(),
        new Promise((_, refuse) => {
          timer = setTimeout(() => refuse(new Error(`still not done after ${limit / 1000} s`)), limit);
        }),
      ]);
    } catch (thrown) {
      error = thrown;
    } finally {
      clearTimeout(timer);
    }
    // A step may time what a person waits for itself (from a moment inside
    // it, such as the last key typed): its `tookMs` is then the step's time.
    record.ms = Number.isFinite(value?.tookMs) ? Math.round(value.tookMs) : Date.now() - from;
    const over = !error && record.ms > budget;
    record.status = error ? 'failed' : over ? 'over budget' : 'ok';
    record.shots = [];
    for (const one of people) {
      const file = await shoot(one, `${String(index).padStart(2, '0')}-${slug(title)}-${slug(one.name)}.png`);
      if (file) record.shots.push({person: one.name, file});
    }
    // What every page said while the step ran; its complaints are named on
    // the step itself.
    const said = consoles.map(([one, at]) => [one.name, one.tab.log.slice(at)]);
    record.complaints = said.flatMap(([name, lines]) => lines.filter(complaint).slice(-10).map(line => `${name}: ${line.slice(0, 400)}`));
    if (error || over) {
      record.why = error ? String(error?.message ?? error) : `took ${duration(record.ms)} of a ${duration(budget)} budget`;
      const base = `evidence/${String(index).padStart(2, '0')}-${slug(title)}`;
      writeFileSync(join(out, `${base}-console.log`),
        said.map(([name, lines]) => `=== ${name}'s page\n${lines.join('\n')}`).join('\n\n'));
      writeFileSync(join(out, `${base}-server.log`), loggedSince(marks) || '(the services logged nothing while this step ran)');
      record.evidence = {console: `${base}-console.log`, server: `${base}-server.log`};
      if (error?.stack) writeFileSync(join(out, `${base}-error.txt`), error.stack);
    }
    console.log(`STEP ${index} ${record.status.toUpperCase()} ${(record.ms / 1000).toFixed(1)}s/${(budget / 1000).toFixed(1)}s ${title}${record.why ? `: ${record.why}` : ''}`);
    if (error) {
      // The check's log is kept longer than its evidence: what the pages and
      // the services said while the step ran goes there too, the tail of each.
      const tail = (text, lines) => text.split('\n').slice(-lines).join('\n');
      for (const [name, lines] of said) console.log(`--- ${name}'s page said while it ran (last 60 lines)\n${tail(lines.join('\n'), 60)}`);
      console.log(`--- the services logged while it ran (last 150 lines)\n${tail(loggedSince(marks) || '(nothing)', 150)}`);
      if (error?.stack) console.log(`--- where it stopped\n${error.stack}`);
      stopped = record;
      throw new StoryStopped(record.why);
    }
    return value;
  };

  /** A measured moment of the story, such as a page's load, in ms. */
  const timing = (name, ms) => { timings[name] = Math.round(ms); };

  /** A measured cost held to its target: the story's own (budgets.json
   * `measures[id]`) or the web page's (`web`). A target with `atLeast` is a
   * floor, such as a rate; any other is a ceiling. */
  const measure = (name, value, detail = '') => {
    const goal = BUDGETS.measures?.[id]?.[name] ?? BUDGETS.web?.[name];
    const target = goal?.target;
    if (!Number.isFinite(target)) throw new Error(`budgets.json has no target ${name} for ${id} or the web page`);
    const ok = Number.isFinite(value) && (goal.atLeast ? value >= target : value <= target);
    const shown = Number.isFinite(value) ? Math.round(value * 100) / 100 : null;
    measures.push({name, value: shown, target: goal.atLeast ? `≥ ${target}` : target, ok, detail: detail || goal.what || ''});
    console.log(`MEASURE ${ok ? 'ok' : 'MISSED'} ${name}=${value} (target ${goal.atLeast ? '≥ ' : ''}${target})${detail ? ` ${detail}` : ''}`);
    return value;
  };

  /** The web page's costs, measured as a person meets them. */
  const web = {
    /** `one` opening `url` in their fresh browser: first content, and the
     * script it sent. Resolves once the page has drawn its first content. */
    coldLoad: async (one, url) => {
      one.bytes = {script: 0, wasm: 0};
      await one.tab.navigate(url);
      const first = await one.tab.waitFor(FIRST_CONTENT, {timeout: 60_000}).catch(() => NaN);
      measure('cold-load-ms', first, one.name);
      return first;
    },
    /** The script the page sent until now on its cold load, compressed. */
    scriptSize: one => {
      measure('script-kb', one.bytes.script / 1024, `${one.name}; WebAssembly ${Math.round(one.bytes.wasm / 1024)} KB`);
    },
    /** `one` opening the page again in the same browser. */
    warmLoad: async one => {
      await one.tab.reload();
      await sleep(200);
      const first = await one.tab.waitFor(FIRST_CONTENT, {timeout: 60_000}).catch(() => NaN);
      measure('warm-load-ms', first, one.name);
      return first;
    },
    signIn: (one, ms) => measure('sign-in-ms', ms, one.name),
    openApp: (one, ms, app) => measure('open-app-ms', ms, `${one.name}, ${app}`),
    action: (one, ms, what) => measure('action-ms', ms, `${one.name}, ${what}`),
    /** `one` typing `text` as a person does, a key every `gap` ms, each key
     * timed to the frame that shows it. Resolves to when the last key went. */
    type: async (one, text, gap = 120) => {
      await one.tab.evaluate(KEY_TO_PAINT);
      await one.tab.evaluate('globalThis.__keyPaint.length = 0');
      let last = Date.now();
      for (const ch of text) {
        last = Date.now();
        await Promise.all([
          one.tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch}),
          one.tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch}),
        ]);
        await sleep(gap);
      }
      await sleep(Math.max(gap, 500));
      const paints = await one.tab.evaluate('JSON.stringify(globalThis.__keyPaint)').then(JSON.parse);
      const shown = `${one.name}, ${paints.length} of ${[...text].length} keys painted`;
      measure('key-to-paint-p50-ms', paints.length ? percentile(paints, 50) : NaN, shown);
      measure('key-to-paint-p90-ms', paints.length ? percentile(paints, 90) : NaN, shown);
      return last;
    },
  };

  /** Stop the recordings, encode each person's video, and write story.json.
   * `error` is what stopped the story outside any step. */
  const finish = async ({error = null, extra = {}, extraOk = true} = {}) => {
    for (const one of people) await one.tab.send('Page.stopScreencast').catch(() => {});
    for (const one of people) await one.launched.close().catch(closing => console.error(`closing ${one.name}'s browser: ${closing.message}`));
    const videos = [];
    for (const one of people) {
      const video = encode(one);
      if (video) videos.push({person: one.name, file: video, captions: caption(one, video)});
    }
    rmSync(join(out, 'frames'), {recursive: true, force: true});
    // Where it broke: the step it stopped at, or else the first over its budget.
    const failedStep = steps.find(record => record.status === 'failed') ?? steps.find(record => record.status !== 'ok') ?? null;
    const story = {
      id,
      at: new Date(began).toISOString(),
      seconds: Math.round((Date.now() - began) / 1000),
      completed: !stopped && !error,
      withinBudget: steps.every(record => record.status === 'ok') && measures.every(entry => entry.ok) && extraOk,
      brokeAt: failedStep ? {index: failedStep.index, title: failedStep.title, why: failedStep.why} : null,
      error: error ? String(error?.stack ?? error).slice(0, 4000) : null,
      steps,
      timings,
      measures,
      videos,
      ...extra,
    };
    writeFileSync(join(out, 'story.json'), JSON.stringify(story, null, 2));
    // The same numbers for machines: each step's time against its budget,
    // the web page's costs against their targets, and the story's moments.
    writeFileSync(join(out, 'timings.json'), JSON.stringify({
      story: id,
      at: story.at,
      revision: process.env.T_COMMIT ?? process.env.SOURCE_REVISION ?? null,
      steps: steps.map(({title, ms, budgetMs, status}) => ({title, ms, budgetMs, status})),
      web: measures,
      moments: timings,
      ...(extra.growth ? {growth: extra.growth} : {}),
    }, null, 2));
    return story;
  };

  /** A person's frames as a video at the pace they were drawn, or null where
   * no ffmpeg is at hand (the step screenshots still tell the story). */
  const encode = one => {
    if (one.kept.length < 2) return null;
    if (spawnSync('ffmpeg', ['-version'], {stdio: 'ignore'}).status !== 0) {
      console.error('no ffmpeg: the recordings stay screenshots');
      return null;
    }
    // Each frame shown until the next was drawn, the last for a second; a
    // page that drew nothing for minutes is held, not skipped.
    const list = one.kept.map((frame, at) => {
      const next = one.kept[at + 1];
      const seconds = next ? Math.max(0.01, next.at - frame.at) : 1;
      return `file '${frame.file}'\nduration ${seconds.toFixed(3)}`;
    });
    list.push(`file '${one.kept.at(-1).file}'`);
    writeFileSync(join(one.frames, 'frames.txt'), `ffconcat version 1.0\n${list.join('\n')}\n`);
    mkdirSync(join(out, 'video'), {recursive: true});
    const file = `video/${slug(one.name)}.webm`;
    const made = spawnSync('ffmpeg', ['-y', '-loglevel', 'error', '-f', 'concat', '-safe', '0', '-i', join(one.frames, 'frames.txt'),
      '-vf', 'fps=10,scale=trunc(iw/2)*2:trunc(ih/2)*2', '-c:v', 'libvpx', '-b:v', '600k', '-deadline', 'realtime', '-cpu-used', '8',
      join(out, file)], {encoding: 'utf8'});
    if (made.status !== 0) {
      console.error(`encoding ${one.name}'s video failed: ${made.stderr.slice(-600)}`);
      return null;
    }
    return file;
  };

  /** The steps as captions over a person's video (WebVTT beside it): each
   * step's name from the moment it began on that person's screen until the
   * next began, so whoever watches knows what the person is trying to do. */
  const caption = (one, video) => {
    const start = one.kept[0].at;
    const end = one.kept.at(-1).at - start + 1;
    const clock = seconds => {
      const ms = Math.round(Math.max(0, seconds) * 1000);
      const pad = (n, w = 2) => String(n).padStart(w, '0');
      return `${pad(Math.floor(ms / 3_600_000))}:${pad(Math.floor(ms / 60_000) % 60)}:${pad(Math.floor(ms / 1000) % 60)}.${pad(ms % 1000, 3)}`;
    };
    const cues = steps.map((record, at) => {
      const from = (began + record.atMs) / 1000 - start;
      const next = steps[at + 1];
      const to = next ? (began + next.atMs) / 1000 - start : end;
      const result = record.status === 'ok' ? '' : ` (${record.status})`;
      return {from, to, text: `${record.index}. ${record.title}${result}`};
    }).filter(cue => cue.to > 0 && cue.from < end && cue.to > cue.from);
    if (!cues.length) return null;
    const file = video.replace(/\.webm$/, '.vtt');
    writeFileSync(join(out, file), `WEBVTT\n\n${cues.map(cue => `${clock(cue.from)} --> ${clock(cue.to)}\n${cue.text}`).join('\n\n')}\n`);
    return file;
  };

  return {person, step, timing, measure, web, finish, get stopped() { return stopped; }};
}
