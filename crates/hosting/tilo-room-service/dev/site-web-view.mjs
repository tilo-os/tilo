#!/usr/bin/env node
// A site's web view, as a person opening it in a browser sees it: headless
// Chrome opens the site (it joins its home workspace through the door, as
// every visitor's page does) and reports, as one JSON line on stdout, the
// room the page opened and, with --expect, whether that room's Chat shows a
// message reading the given text. What dist/dev/site-door-acceptance.py
// compares with the room and the post of its Tilo computers.
//
// usage: node crates/hosting/tilo-room-service/dev/site-web-view.mjs
//          --site URL [--expect TEXT] [--out DIR] [--timeout S]
//
// The report: {"entry", "room", "shows"} ("shows" only with --expect; when it
// is false, also "why": the step at which the page's Chat found no channel,
// or the channel it read, "reads": what its Chat holds there, and
// "channels": the page's channel state). Exits
// 1 when the page did not open, 2 when Chrome cannot draw here.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {chromeFlags} from '../../../../scripts/perf/browser_qualification.mjs';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {CHAT_CHANNEL, CHAT_WHY, chatShows, open, page} from './auth-e2e.mjs';

/** In the page: what its Chat holds for `channel` (how many messages, the
 * latest few texts), or why it could not say. */
const CHANNEL_READS = ({window, channel}) => `(async () => {
  let result;
  try {
    result = await tiloMount.invokeAction(${JSON.stringify(`chat.workspace/channels/${channel}`)}, 'read-messages',
      {pageSize: 32}, {window: ${JSON.stringify(window)}});
  } catch (error) {
    return {refused: String(error?.message ?? error)};
  }
  const messages = result?.output?.value?.messages;
  return {state: result?.state ?? null, error: result?.error ?? null,
    count: Array.isArray(messages) ? messages.length : null,
    latest: Array.isArray(messages) ? messages.slice(-3).map(message => message.text) : null};
})()`;

function parse(argv) {
  const options = {timeout: 120};
  for (let index = 0; index < argv.length; index += 2) {
    const [flag, value] = [argv[index], argv[index + 1]];
    if (value === undefined) throw new Error(`${flag} needs a value`);
    if (flag === '--site') options.site = value;
    else if (flag === '--expect') options.expect = value;
    else if (flag === '--out') options.out = value;
    else if (flag === '--timeout') options.timeout = Number(value);
    else throw new Error(`unknown option ${flag}`);
  }
  if (!options.site) throw new Error('--site is required');
  return options;
}

async function until(condition, timeout, every = 1000) {
  const end = Date.now() + timeout;
  for (;;) {
    const value = await condition().catch(() => null);
    if (value) return value;
    if (Date.now() > end) return null;
    await sleep(every);
  }
}

async function main() {
  let options;
  try {
    options = parse(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exit(2);
  }
  if (options.out) mkdirSync(options.out, {recursive: true});
  const timeout = options.timeout * 1000;
  const site = new URL(options.site);
  // Chrome's own WebGPU on the CPU, as the qualification runs it.
  const browser = await launch({args: chromeFlags('swiftshader', process.getuid?.())});
  let report = {entry: 'not loaded', room: null};
  try {
    const person = await browser.context();
    const tab = await page(browser, person);
    // The page keeps the membership it holds for the site's host; which
    // host that is, it learns from the site: any is accepted here.
    const opened = await open(tab, site.href, {relay: '', timeout});
    const room = await tab.evaluate(`(() => {
      const match = /\\/w\\/([0-9a-f-]{36})/.exec(location.hash);
      return match ? match[1] : null;
    })()`).catch(() => null);
    report = {entry: opened.entry, room, status: opened.status};
    if (opened.entry === 'unsupported') {
      console.log(JSON.stringify(report));
      process.exit(2);
    }
    if (opened.entry === 'open' && options.expect !== undefined) {
      const channel = await until(() => tab.evaluate(CHAT_CHANNEL), timeout);
      report.shows = Boolean(channel)
        && Boolean(await until(() => chatShows(tab, channel.channel, options.expect), timeout));
      if (!report.shows) {
        report.why = channel ?? await tab.evaluate(CHAT_WHY).catch(error => String(error?.message ?? error));
        if (channel) report.reads = await tab.evaluate(CHANNEL_READS(channel)).catch(error => String(error?.message ?? error));
        report.channels = await tab.evaluate('globalThis.tiloChannels?.() ?? ""').catch(error => String(error?.message ?? error));
      }
    }
    if (options.out) {
      const shot = await tab.screenshot().catch(() => null);
      if (shot) writeFileSync(join(options.out, `web-view-${Date.now()}.png`), shot);
    }
  } finally {
    await browser.close?.();
  }
  console.log(JSON.stringify(report));
  process.exit(report.entry === 'open' ? 0 : 1);
}

await main();
