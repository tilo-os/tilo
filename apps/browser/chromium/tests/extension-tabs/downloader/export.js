// A settings export, as extensions offer one from their pages: the file is
// made here, as a blob, and downloaded from its address.
const url = URL.createObjectURL(new Blob(['setting,on\n'], {type: 'text/csv'}));
chrome.downloads.download({url, filename: 'settings.csv'})
  .then((id) => { document.title = 'exported:' + id; }, (error) => { document.title = 'failed:' + error.message; });
