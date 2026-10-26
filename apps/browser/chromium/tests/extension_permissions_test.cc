#include "extension_permissions.h"
#include <cassert>
#include <string>
#include <utility>
#include <vector>

using tilo::ExtensionPermissions;
using tilo::PatternCovers;
using tilo::PermissionSet;

int main() {
  // A pattern covers another when it matches every page the other does.
  assert(PatternCovers("<all_urls>", "https://example.com/*"));
  assert(PatternCovers("*://*/*", "https://example.com/docs/*"));
  assert(PatternCovers("https://*.example.com/*", "https://example.com/*"));
  assert(PatternCovers("https://*.example.com/*", "https://news.example.com/*"));
  assert(PatternCovers("https://*.example.com/*", "https://*.news.example.com/*"));
  assert(PatternCovers("http://127.0.0.1/*", "http://127.0.0.1/page"));
  assert(!PatternCovers("https://example.com/*", "https://*.example.com/*"));
  assert(!PatternCovers("https://example.com/*", "http://example.com/*"));
  assert(!PatternCovers("https://example.com/docs/*", "https://example.com/*"));
  assert(!PatternCovers("*://*/*", "file:///*"));
  assert(!PatternCovers("https://*.example.com/*", "https://*/*"));
  assert(!PatternCovers("https://example.com/*", "<all_urls>"));
  // Not a pattern: covers nothing, and nothing covers it.
  assert(!PatternCovers("https://example.com", "https://example.com/*"));
  assert(!PatternCovers("<all_urls>", "chrome://settings/*"));
  assert(!PatternCovers("<all_urls>", "https://ex*ample.com/*"));
  // A pattern naming no port covers every port, as in Chrome; one naming a
  // port covers that port alone.
  assert(PatternCovers("http://localhost/*", "http://localhost:8080/*"));
  assert(PatternCovers("*://*.example.com/*", "https://a.example.com:8443/*"));
  assert(PatternCovers("http://localhost:*/*", "http://localhost:8080/*"));
  assert(PatternCovers("http://localhost:8080/*", "http://localhost:8080/app/*"));
  assert(!PatternCovers("http://localhost:8080/*", "http://localhost:9090/*"));
  assert(!PatternCovers("http://localhost:8080/*", "http://localhost/*"));
  assert(PatternCovers("http://[::1]/*", "http://[::1]:8080/*"));
  assert(!PatternCovers("http://localhost:80a/*", "http://localhost:80a/*"));
  // A page is on a pattern's port: its own, or its scheme's.
  assert(tilo::SplitPatternHost("[::1]") == std::make_pair(std::string("[::1]"), std::string()));
  assert(tilo::SplitPatternHost("*.example.com:*") == std::make_pair(std::string("*.example.com"), std::string()));
  assert(tilo::PortMatches("", "http", "8080"));
  assert(tilo::PortMatches("8080", "http", "8080"));
  assert(!tilo::PortMatches("8080", "http", "9090"));
  assert(tilo::PortMatches("80", "http", "") && tilo::PortMatches("443", "wss", ""));
  assert(!tilo::PortMatches("8080", "https", ""));

  const std::string id = "abcdefghijklmnopabcdefghijklmnop";
  ExtensionPermissions permissions;
  permissions.SetRequired(id, {"storage"}, {"https://example.com/*"});
  permissions.SetOptional(id, PermissionSet{{"tabs", "bookmarks"}, {"http://127.0.0.1/*", "https://*.example.org/*"}}, PermissionSet{});

  // What the manifest requires it holds; what it may ask for it does not yet.
  assert(permissions.Has(id, "storage"));
  assert(!permissions.Has(id, "tabs"));
  assert(permissions.Holds(id, PermissionSet{{"storage"}, {"https://example.com/page/*"}}));
  assert(!permissions.Holds(id, PermissionSet{{"tabs"}, {}}));

  // A request names only what it lacks, all of it in the manifest.
  PermissionSet missing;
  assert(!permissions.Ask(id, PermissionSet{{"storage", "tabs"}, {"https://example.com/*", "http://127.0.0.1/*"}}, missing));
  assert((missing.permissions == std::vector<std::string>{"tabs"}));
  assert((missing.origins == std::vector<std::string>{"http://127.0.0.1/*"}));
  // A part of an optional site is asked for alone.
  PermissionSet part;
  assert(!permissions.Ask(id, PermissionSet{{}, {"https://news.example.org/*"}}, part));
  assert((part.origins == std::vector<std::string>{"https://news.example.org/*"}));
  // A site on a port of its own is part of a site the manifest names on
  // every port.
  PermissionSet port;
  assert(!permissions.Ask(id, PermissionSet{{}, {"http://127.0.0.1:8080/*"}}, port));
  assert((port.origins == std::vector<std::string>{"http://127.0.0.1:8080/*"}));
  // Anything else is refused, as Chrome refuses it, and nothing is asked.
  for (const auto& refused : {PermissionSet{{"history"}, {}}, PermissionSet{{}, {"https://other.test/*"}}, PermissionSet{{"tabs"}, {"<all_urls>"}}}) {
    PermissionSet none;
    assert(permissions.Ask(id, refused, none) == std::string(tilo::kOnlyDeclared));
  }
  PermissionSet none;
  assert(permissions.Ask(id, PermissionSet{{}, {"example.com"}}, none));
  // An extension it does not know may ask for nothing.
  assert(permissions.Ask("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", PermissionSet{{"tabs"}, {}}, none));

  // Granted: it holds them, and getAll lists them after what is required.
  const auto added = permissions.Grant(id, missing);
  assert(added.permissions == missing.permissions && added.origins == missing.origins);
  assert(permissions.Grant(id, missing).empty());
  assert(permissions.Has(id, "tabs"));
  assert(permissions.Holds(id, PermissionSet{{"tabs"}, {"http://127.0.0.1/page"}}));
  const auto all = permissions.All(id);
  assert((all.permissions == std::vector<std::string>{"storage", "tabs"}));
  assert((all.origins == std::vector<std::string>{"https://example.com/*", "http://127.0.0.1/*"}));
  assert((permissions.Hosts(id) == all.origins));
  PermissionSet nothing;
  assert(!permissions.Ask(id, PermissionSet{{"tabs"}, {"http://127.0.0.1/*"}}, nothing) && nothing.empty());

  // What is required cannot be removed; then nothing is.
  PermissionSet removed;
  assert(permissions.Remove(id, PermissionSet{{"tabs", "storage"}, {}}, removed) == std::string(tilo::kRequired));
  assert(permissions.Remove(id, PermissionSet{{}, {"https://*/*"}}, removed) == std::string(tilo::kRequired));
  assert(removed.empty() && permissions.Has(id, "tabs"));
  // What was granted can: a broader pattern takes the sites it covers.
  assert(!permissions.Remove(id, PermissionSet{{"tabs", "bookmarks"}, {"http://*/*"}}, removed));
  assert((removed.permissions == std::vector<std::string>{"tabs"}));
  assert((removed.origins == std::vector<std::string>{"http://127.0.0.1/*"}));
  assert(!permissions.Has(id, "tabs") && permissions.Granted(id).empty());

  // Granted in an earlier run: kept while the manifest still offers it.
  permissions.SetOptional(id, PermissionSet{{"tabs"}, {"http://127.0.0.1/*"}}, PermissionSet{{"tabs", "history"}, {"http://127.0.0.1/*", "https://other.test/*"}});
  const auto kept = permissions.Granted(id);
  assert((kept.permissions == std::vector<std::string>{"tabs"}));
  assert((kept.origins == std::vector<std::string>{"http://127.0.0.1/*"}));

  // The Browser takes back what the person revoked: what it held and holds
  // no more is what the extension hears it lost.
  permissions.SetOptional(id, PermissionSet{{"tabs"}, {"http://127.0.0.1/*"}}, PermissionSet{});
  const auto lost = tilo::Without(kept, permissions.Granted(id));
  assert((lost.permissions == std::vector<std::string>{"tabs"}));
  assert((lost.origins == std::vector<std::string>{"http://127.0.0.1/*"}));
  assert(!permissions.Has(id, "tabs") && tilo::Without(permissions.Granted(id), kept).empty());

  permissions.Forget(id);
  assert(!permissions.Has(id, "storage") && permissions.All(id).empty());

  // Signing in asked for later: granted, it counts as the manifest's would,
  // and only then.
  const std::string signer = "ponmlkjihgfedcbaponmlkjihgfedcba";
  permissions.SetRequired(signer, {}, {});
  permissions.SetOptional(signer, PermissionSet{{"identity"}, {}}, PermissionSet{});
  assert(!permissions.Has(signer, "identity"));
  PermissionSet signing;
  assert(!permissions.Ask(signer, PermissionSet{{"identity"}, {}}, signing));
  assert((signing.permissions == std::vector<std::string>{"identity"}));
  permissions.Grant(signer, signing);
  assert(permissions.Has(signer, "identity"));
  permissions.SetOptional(signer, PermissionSet{{"identity"}, {}}, PermissionSet{{"identity"}, {}});
  assert(permissions.Has(signer, "identity"));
  return 0;
}
