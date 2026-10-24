// A worker that injects a script into the frame a message came from, by
// the frame's id (`sender.frameId`), as Bitwarden does for each frame of a
// page; then reaches every frame at once, and one frame's content script.
const heard = new Map();
chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message.hello) return;
  (async () => {
    const [injected] = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id, frameIds: [sender.frameId]},
      files: ['fields.js'],
    });
    heard.set(message.hello, {frame: sender.frameId, injected: injected.result, id: injected.frameId === sender.frameId});
    if (heard.size < 2) return;
    const inner = heard.get('/inner').frame;
    const all = await chrome.scripting.executeScript({
      target: {tabId: sender.tab.id, allFrames: true},
      func: () => location.pathname,
    });
    const where = await chrome.tabs.sendMessage(sender.tab.id, {where: true}, {frameId: inner});
    const missing = await chrome.scripting.executeScript({target: {tabId: sender.tab.id, frameIds: [inner + 100000]}, func: () => 1})
      .then(() => 'ran', (error) => error.message.startsWith('No frame with id'));
    const report = {
      top: heard.get('/framed'),
      inner: {...heard.get('/inner'), frame: inner > 0},
      all: all.map((entry) => [entry.frameId === 0 ? 'main' : entry.frameId === inner ? 'inner' : entry.frameId, entry.result]),
      where,
      missing,
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
