// An ad blocker as uBlock Origin Lite is: its rules block ads and trackers, or
// put a harmless script of its own in place of one (at an address of its own
// for the session, or at its file's), and its button shows how many requests
// they acted on in the page. Chromium counts them; the extension only asks.
chrome.declarativeNetRequest.setExtensionActionOptions({displayActionCountAsBadgeText: true});
