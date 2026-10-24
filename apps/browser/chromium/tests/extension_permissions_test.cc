#include "extension_permissions.h"
#include <cassert>
#include <string>
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

  permissions.Forget(id);
  assert(!permissions.Has(id, "storage") && permissions.All(id).empty());
  return 0;
}
