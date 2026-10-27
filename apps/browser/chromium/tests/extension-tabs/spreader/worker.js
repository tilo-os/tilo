// A worker with no content scripts that injects its script into every frame
// of a page it may script (`allFrames`), as a clipper or an editor's helper
// does, then sends the whole tab a message. It may script the page's site,
// not the other site's frame.
const heard = new Map();
chrome.runtime.onMessage.addListener((message, sender) => {
  if (message.heard) heard.set(message.heard, {frameId: sender.frameId, documentId: sender.documentId});
});
const pause = () => new Promise((resolve) => setTimeout(resolve, 50));
globalThis.spread = async () => {
  const tab = (await chrome.tabs.query({})).find((each) => each.url && new URL(each.url).pathname === '/framed');
  // Once the page and its frames have loaded.
  for (let tries = 0; tries < 200; tries++) {
    const [{result}] = await chrome.scripting.executeScript({target: {tabId: tab.id}, func: () => document.readyState});
    if (result === 'complete') break;
    await pause();
  }
  const injected = await chrome.scripting.executeScript({target: {tabId: tab.id, allFrames: true}, files: ['inject.js']});
  const at = (path) => injected.find((entry) => entry.result === path) || {};
  const everyone = await chrome.tabs.sendMessage(tab.id, {kind: 'everyone'});
  for (let tries = 0; tries < 100 && heard.size < 2; tries++) await pause();
  // Chromium's own names for the frames and their documents.
  const listed = await chrome.webNavigation.getAllFrames({tabId: tab.id});
  const named = (path) => listed.find((frame) => new URL(frame.url).pathname === path) || {};
  return JSON.stringify({
    injected: injected.map((entry) => [entry.result, entry.frameId === named(entry.result).frameId, entry.documentId === named(entry.result).documentId]).sort(),
    inner: at('/inner').frameId > 0,
    everyone: ['/framed', '/inner'].includes(everyone) ? 'a frame' : everyone,
    heard: Object.fromEntries([...heard].map(([path, from]) => [path, from.frameId === at(path).frameId && from.documentId === at(path).documentId]).sort()),
    byFrame: await chrome.tabs.sendMessage(tab.id, {kind: 'everyone'}, {frameId: at('/inner').frameId}).catch((error) => 'error: ' + error.message),
  });
};
