// The person picks a setting in the popup.
chrome.storage.local.set({mode: 'dark'}).then(() => { document.title = 'set'; });
