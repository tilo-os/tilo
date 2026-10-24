// What Chromium tells the extension of downloads, in order, for the test
// to read.
globalThis.heard = [];
chrome.downloads.onCreated.addListener((item) => heard.push({created: item}));
chrome.downloads.onChanged.addListener((delta) => heard.push({changed: delta}));
// A call's outcome: its value, or the error it failed with.
globalThis.outcome = (promise) => promise.then((value) => ({value: value ?? null}), (error) => ({error: error.message}));
