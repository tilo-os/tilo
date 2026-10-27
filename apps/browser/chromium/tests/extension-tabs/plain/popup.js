// Without the tabs permission or access to the site, a tab has no address.
chrome.tabs.query({active: true, currentWindow: true}).then(([tab]) => {
  document.title = 'plain:' + JSON.stringify({found: !!tab, url: tab ? tab.url === undefined : null});
});
