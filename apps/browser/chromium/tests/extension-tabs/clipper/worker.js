// A clipper's worker: it reaches its panel in the page by the frame and the
// document a message from the panel came from, as Evernote Web Clipper
// answers its panel, and the page's frames by their documents.
const heard = new Map();
const frames = ['/framed', '/inner', '/outer', '/panel.html'];
const settled = (promise) => promise.then((value) => value, (error) => 'error: ' + error.message);
chrome.runtime.onMessage.addListener((message, sender) => {
  if (!message.hello || !sender.tab) return;
  heard.set(message.hello, {frameId: sender.frameId, documentId: sender.documentId, url: sender.url});
  if (!frames.every((path) => heard.has(path))) return;
  const tabId = sender.tab.id;
  const at = (path) => heard.get(path);
  const named = (entry) => frames.find((path) => at(path).frameId === entry.frameId) || entry.frameId;
  (async () => {
    const panel = at('/panel.html');
    const outer = at('/outer');
    const report = {
      tab: tabId,
      byFrame: await settled(chrome.tabs.sendMessage(tabId, {ping: true}, {frameId: panel.frameId})),
      byDocument: await settled(chrome.tabs.sendMessage(tabId, {ping: true}, {documentId: panel.documentId})),
      again: await settled(chrome.tabs.sendMessage(tabId, {ping: true}, {frameId: panel.frameId})),
      mismatched: await settled(chrome.tabs.sendMessage(tabId, {ping: true}, {frameId: outer.frameId, documentId: panel.documentId})),
      outer: await settled(chrome.tabs.sendMessage(tabId, {where: true}, {documentId: outer.documentId})),
      documents: await settled(chrome.scripting.executeScript({
        target: {tabId, documentIds: [at('/inner').documentId, outer.documentId]},
        func: () => location.pathname,
      }).then((entries) => entries.map((entry) => [named(entry), entry.result, entry.documentId === at(entry.result).documentId]).sort())),
      all: await settled(chrome.scripting.executeScript({target: {tabId, allFrames: true}, func: () => location.pathname})
        .then((entries) => entries.map((entry) => [named(entry), entry.result]).sort())),
      intoPanel: await settled(chrome.scripting.executeScript({target: {tabId, frameIds: [panel.frameId]}, func: () => 1})),
      both: await settled(chrome.scripting.executeScript({target: {tabId, allFrames: true, frameIds: [0]}, func: () => 1})),
      allAndDocuments: await settled(chrome.scripting.executeScript({target: {tabId, allFrames: true, documentIds: []}, func: () => 1})),
      idsAndDocuments: await settled(chrome.scripting.executeScript({target: {tabId, frameIds: [0], documentIds: [outer.documentId]}, func: () => 1})),
      malformed: await settled(chrome.scripting.executeScript({target: {tabId, documentIds: ['nope']}, func: () => 1})),
      gone: await settled(chrome.scripting.executeScript({target: {tabId, documentIds: ['0123456789ABCDEF0123456789ABCDEF']}, func: () => 1})),
      ids: frames.slice(1).every((path) => Number.isInteger(at(path).frameId) && at(path).frameId > 0) && at('/framed').frameId === 0,
    };
    await fetch(new URL('/report/clipper', at('/framed').url), {method: 'POST', body: JSON.stringify(report)});
  })();
});
