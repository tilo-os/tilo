// A worker that injects a script into the frame a message came from, by
// the frame's id (`sender.frameId`), as Bitwarden does for each frame of a
// page: the page's, one of the same site and one another site's process
// renders. Then it reaches every frame at once, and each frame's content
// script.
const heard = new Map();
const frames = ['/framed', '/inner', '/outer'];
chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message.hello) return;
  (async () => {
    const [injected] = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id, frameIds: [sender.frameId]},
      files: ['fields.js'],
    });
    heard.set(message.hello, {frame: sender.frameId, injected: injected.result, id: injected.frameId === sender.frameId});
    if (!frames.every((path) => heard.has(path))) return;
    const id = (path) => heard.get(path).frame;
    const all = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id, allFrames: true},
      func: () => location.pathname,
    });
    const where = {};
    for (const path of frames.slice(1)) {
      where[path] = await chrome.tabs.sendMessage(sender.tab.id, {where: true}, {frameId: id(path)});
    }
    // A script as large as a password manager's form filler (near 2 MiB),
    // into the frame of another site.
    const [large] = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id, frameIds: [id('/outer')]},
      func: (pad) => location.pathname + ':' + pad.length,
      args: ['x'.repeat(1500000)],
    });
    const missing = await chrome.scripting.executeScript({target: {tabId: sender.tab.id, frameIds: [id('/inner') + 100000]}, func: () => 1})
      .then(() => 'ran', (error) => error.message.startsWith('No frame with id'));
    const named = (frameId) => (frames.find((path) => id(path) === frameId) || frameId);
    // The frame of the same site goes to a page of the other site the
    // content script does not match: another process renders it now, and it
    // keeps its id, but the extension has no world there. A script into
    // every frame still runs in those it can reach.
    const [{result: moved}] = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id},
      func: () => new Promise((resolve) => {
        const [inner, outer] = document.querySelectorAll('iframe');
        inner.addEventListener('load', () => resolve(true), {once: true});
        setTimeout(() => resolve(false), 10000);
        inner.src = new URL('/unmatched', outer.src).href;
      }),
    });
    const after = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id, allFrames: true},
      func: () => location.pathname,
    }).then((entries) => entries.map((entry) => [named(entry.frameId), entry.result]).sort(), (error) => 'failed: ' + error.message);
    const report = {
      frames: Object.fromEntries(frames.map((path) => [path, {...heard.get(path), frame: path === '/framed' ? id(path) === 0 : id(path) > 0}])),
      all: all.map((entry) => [named(entry.frameId), entry.result]).sort(),
      where,
      large: large.result,
      missing,
      moved,
      after,
    };
    await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id},
      func: (text) => { document.title = 'framed:' + text; },
      args: [JSON.stringify(report)],
    });
  })().catch((error) => chrome.scripting.executeScript({
    target: {tabId: sender.tab.id},
    func: (text) => { document.title = 'framed-failed:' + text; },
    args: [error.message],
  }));
});
