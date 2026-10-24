// Signs in on the page the test names, as an extension signs in to its
// service, and keeps each outcome for the test to read.
globalThis.signIns = [];
globalThis.signIn = (details) => {
  chrome.identity.launchWebAuthFlow(details).then(
    (url) => signIns.push({url}),
    (error) => signIns.push({error: error.message}));
  return 'started';
};
