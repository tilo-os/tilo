// A stand-in for a site's analytics script, as uBlock Origin Lite's are: the
// page's calls to its API do nothing, rather than throw.
window.ga = function () {};
window.ga.standIn = true;
