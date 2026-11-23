// A welcome page in a tab of its own keeps its tab's id, and a blank tab's,
// as extensions keep the ids of tabs they act on later.
(async () => {
  const own = await chrome.tabs.getCurrent();
  const blank = (await chrome.tabs.query({})).find((tab) => tab.url === 'about:blank');
  await chrome.storage.local.set({welcome: own.id, blank: blank.id});
  document.title = 'welcome:' + JSON.stringify({own: Number.isInteger(own.id), blank: Number.isInteger(blank.id), distinct: own.id !== blank.id});
})().catch((error) => { document.title = 'failed:' + error.message; });
