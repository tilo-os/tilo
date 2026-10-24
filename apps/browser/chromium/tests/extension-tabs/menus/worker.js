// Items for each context of a page's menu, made once when installed as a
// real extension makes them. Choosing one marks the page it was chosen on,
// which activeTab lets it script, with what the extension was told.
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({id: 'page', title: 'Mark the page', contexts: ['page']});
  chrome.contextMenus.create({id: 'selection', title: 'Look up "%s"', contexts: ['selection']});
  chrome.contextMenus.create({id: 'link', title: 'Mark the link', contexts: ['link']});
  chrome.contextMenus.create({id: 'image', title: 'Mark the image', contexts: ['image']});
  chrome.contextMenus.create({id: 'remember', title: 'Remember the page', type: 'checkbox'});
  // Never shown: on other sites only, and taken away again.
  chrome.contextMenus.create({id: 'elsewhere', title: 'Elsewhere', contexts: ['all'], documentUrlPatterns: ['https://example.invalid/*']});
  chrome.contextMenus.create({id: 'gone', title: 'Gone', contexts: ['all']});
  chrome.contextMenus.remove('gone');
  // Renamed once made.
  chrome.contextMenus.create({id: 'renamed', title: 'Before', contexts: ['all']});
  chrome.contextMenus.update('renamed', {title: 'Everywhere'});
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (seen) => { document.title = 'chosen:' + JSON.stringify(seen); },
    args: [{info, url: tab.url}],
  });
});
