// Asks for signing in only later (`optional_permissions`), then signs in on
// the page the test names and keeps each outcome for the test to read.
globalThis.signIns = [];
globalThis.signIn = (details) => {
  chrome.identity.launchWebAuthFlow(details).then(
    (url) => signIns.push({url}),
    (error) => signIns.push({error: error.message}));
  return 'started';
};
