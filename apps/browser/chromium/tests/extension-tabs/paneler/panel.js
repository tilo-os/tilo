// The panel is no tab, and its window is the page beside it.
(async () => {
  const current = await chrome.tabs.getCurrent();
  const window = await chrome.windows.getCurrent({populate: true});
  document.title = 'panel:' + JSON.stringify({tab: current === undefined, window: window.tabs[0].url});
})();
