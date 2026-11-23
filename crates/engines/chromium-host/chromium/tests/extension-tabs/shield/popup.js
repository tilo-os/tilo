// The popup shows the site it was opened over and how many trackers were
// blocked there. Asked to (`?allow`), it turns itself off for the site,
// reloads the page and closes at once, as Privacy Badger's "Disable for this
// site" does; asked to mark the page (`?mark`), it starts a script there and
// closes at once, as uBlock Origin Lite's element zapper does.
(async () => {
  const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
  const site = new URL(tab.url).origin;
  const count = await chrome.runtime.sendMessage({count: true, tabId: tab.id});
  document.title = 'shield:' + JSON.stringify({site, blocked: count});
  if (location.search === '?allow') {
    const allowed = await chrome.runtime.sendMessage({allow: site});
    if (allowed !== true) throw new Error(String(allowed));
    chrome.tabs.reload(tab.id);
    window.close();
  } else if (location.search === '?mark') {
    chrome.scripting.executeScript({target: {tabId: tab.id}, files: ['mark.js']});
    window.close();
  }
})().catch((error) => { document.title = 'failed:' + error.message; });
