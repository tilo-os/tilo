"use strict"
// A worker that knows itself by its address, as store extensions do: one
// tells its background from its pages by the word in its URL, and loads its
// helpers relative to itself. A click reports what it found to the page.
importScripts('helper.js');
const strict = (function () { return this === undefined; })();
const own = chrome.runtime.getManifest().background.service_worker;
chrome.action.onClicked.addListener(async (tab) => {
  const found = {
    background: location.href.indexOf('background') > -1,
    path: location.pathname,
    manifest: own,
    helper: self.helper === 'beside',
    strict,
  };
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (found) => { document.title = 'located:' + found; },
    args: [JSON.stringify(found)],
  });
});
