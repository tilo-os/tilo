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
  // A radio group, the second checked by an update; a submenu; an item
  // shown greyed; an ampersand.
  chrome.contextMenus.create({id: 'light', title: 'Light', type: 'radio'});
  chrome.contextMenus.create({id: 'dark', title: 'Dark', type: 'radio'});
  chrome.contextMenus.update('dark', {checked: true});
  chrome.contextMenus.create({id: 'translate', title: 'Translate'});
  chrome.contextMenus.create({id: 'german', title: 'To German', parentId: 'translate'});
  chrome.contextMenus.create({id: 'off', title: 'Not here', enabled: false});
  chrome.contextMenus.create({id: 'share', title: 'Save & share'});
  // The toolbar button's menu: a submenu with a checkbox.
  chrome.contextMenus.create({id: 'sort', title: 'Sort', contexts: ['action']});
  chrome.contextMenus.create({id: 'by-name', title: 'By name', type: 'checkbox', checked: true, parentId: 'sort', contexts: ['action']});
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (seen) => { document.title = 'chosen:' + JSON.stringify(seen); },
    args: [{info, url: tab.url}],
  });
});
