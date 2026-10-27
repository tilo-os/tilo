// A second blocker, which stops beacons and may read what its rules matched
// (declarativeNetRequestFeedback), as AdGuard may; its button too shows how
// many requests its rules acted on in the page.
chrome.declarativeNetRequest.setExtensionActionOptions({displayActionCountAsBadgeText: true});
