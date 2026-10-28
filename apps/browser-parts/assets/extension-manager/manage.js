// Turn extensions on and off as Chrome's own extensions page does, which
// keeps their data: `manage.html?on=<id>,<id>&off=<id>`. An identifier the
// browser does not know is skipped. The title says when it is done.
const query = new URLSearchParams(location.search);
const ids = (key) => (query.get(key) || "").split(",").filter(Boolean);
Promise.allSettled([
  ...ids("on").map((id) => chrome.management.setEnabled(id, true)),
  ...ids("off").map((id) => chrome.management.setEnabled(id, false)),
]).then((results) => {
  const failed = results.filter((result) => result.status === "rejected").length;
  document.title = failed ? `done, ${failed} unknown` : "done";
});
