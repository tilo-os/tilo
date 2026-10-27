#pragma once
// Optional permissions for Chrome extensions (`chrome.permissions`).
//
// An extension's manifest may name access it asks for later, at a moment
// the person can make sense of (`optional_permissions`,
// `optional_host_permissions`). Chromium asks the person in a dialog of the
// window the extension's page is in, and Tilo's pages are in none: it
// answers "Could not find an active window." Here the Browser asks instead
// (the host tells it of a request, extension_tabs.h), keeps what the person
// granted with the extension, and tells the host of it when the extension
// starts. This keeps, for each extension, what its manifest requires, what
// it may ask for, and what of that it was granted, and answers
// `request`, `contains`, `getAll` and `remove` as Chrome does.
//
// Plain C++: the host's URL parsing is not needed for patterns, so this is
// tested alone (tests/extension_permissions_test.cc).
#include <algorithm>
#include <map>
#include <optional>
#include <set>
#include <string>
#include <tuple>
#include <vector>

#include "match_host.h"

namespace tilo {

// A match pattern's parts (`*://*.example.com/*`), or `<all_urls>`. The
// port is empty where the pattern names none, which is every port.
struct PatternParts {
  bool all = false;
  std::string scheme, host, port, path;
};

// The parts of a match pattern Chrome accepts for a site, or none.
inline std::optional<PatternParts> ParsePermissionPattern(const std::string& pattern) {
  if (pattern == "<all_urls>") return PatternParts{true, {}, {}, {}, {}};
  const auto separator = pattern.find("://");
  if (separator == std::string::npos) return std::nullopt;
  PatternParts parts;
  parts.scheme = pattern.substr(0, separator);
  static const std::set<std::string> schemes{"*", "http", "https", "file", "ws", "wss"};
  if (!schemes.count(parts.scheme)) return std::nullopt;
  const auto slash = pattern.find('/', separator + 3);
  if (slash == std::string::npos) return std::nullopt;
  std::tie(parts.host, parts.port) = SplitPatternHost(pattern.substr(separator + 3, slash - separator - 3));
  if (!ValidPatternPort(parts.port)) return std::nullopt;
  parts.path = pattern.substr(slash);
  if (parts.host.empty() && parts.scheme != "file") return std::nullopt;
  // A wildcard only as the whole host or its first label.
  const auto star = parts.host.find('*');
  if (star != std::string::npos && !(parts.host == "*" || (star == 0 && parts.host.rfind("*.", 0) == 0 && parts.host.find('*', 1) == std::string::npos)))
    return std::nullopt;
  return parts;
}

// `*` matches any run of characters, also in `text`, where it is a
// pattern's own wildcard: `/*` covers `/docs/*`.
inline bool PathCovers(const std::string& outer, const std::string& inner) {
  size_t p = 0, t = 0, star = std::string::npos, mark = 0;
  while (t < inner.size()) {
    if (p < outer.size() && outer[p] == '*') { star = p++; mark = t; }
    else if (p < outer.size() && outer[p] == inner[t]) { ++p; ++t; }
    else if (star != std::string::npos) { p = star + 1; t = ++mark; }
    else return false;
  }
  while (p < outer.size() && outer[p] == '*') ++p;
  return p == outer.size();
}

// Whether every page the pattern `inner` matches, `outer` matches too.
inline bool PatternCovers(const std::string& outer, const std::string& inner) {
  const auto a = ParsePermissionPattern(outer), b = ParsePermissionPattern(inner);
  if (!a || !b) return false;
  // Every scheme a pattern may name is among `<all_urls>`'s.
  if (a->all) return true;
  if (b->all) return false;
  if (a->scheme == "*" ? !(b->scheme == "*" || b->scheme == "http" || b->scheme == "https") : a->scheme != b->scheme) return false;
  // A pattern naming a port covers only patterns on that port.
  if (!a->port.empty() && a->port != b->port) return false;
  if (a->host != "*") {
    if (a->host.rfind("*.", 0) == 0) {
      const std::string base = a->host.substr(2);
      std::string host = b->host.rfind("*.", 0) == 0 ? b->host.substr(2) : b->host;
      if (b->host == "*") return false;
      if (host != base && !(host.size() > base.size() && host.compare(host.size() - base.size() - 1, std::string::npos, "." + base) == 0)) return false;
    } else if (a->host != b->host) {
      return false;
    }
  }
  return PathCovers(a->path, b->path);
}

// Whether two patterns may match a page in common.
inline bool PatternsOverlap(const std::string& a, const std::string& b) {
  return PatternCovers(a, b) || PatternCovers(b, a);
}

// API permissions and site patterns, as `chrome.permissions` names them.
struct PermissionSet {
  std::vector<std::string> permissions, origins;
  bool empty() const { return permissions.empty() && origins.empty(); }
};

// What of `from` is not in `without`.
inline PermissionSet Without(const PermissionSet& from, const PermissionSet& without) {
  PermissionSet left;
  const auto in = [](const std::vector<std::string>& list, const std::string& value) {
    return std::find(list.begin(), list.end(), value) != list.end();
  };
  for (const auto& permission : from.permissions) if (!in(without.permissions, permission)) left.permissions.push_back(permission);
  for (const auto& origin : from.origins) if (!in(without.origins, origin)) left.origins.push_back(origin);
  return left;
}

inline constexpr char kOnlyDeclared[] = "Only permissions specified in the manifest may be requested.";
inline constexpr char kRequired[] = "You cannot remove required permissions.";

class ExtensionPermissions {
 public:
  // What the manifest requires: granted as it is added.
  void SetRequired(const std::string& id, std::set<std::string> permissions, std::vector<std::string> hosts) {
    auto& entry = entries_[id];
    entry.required = PermissionSet{{permissions.begin(), permissions.end()}, std::move(hosts)};
  }
  // What it may ask for, and what of that the person granted. A grant the
  // manifest no longer offers (an update dropped it) is none.
  void SetOptional(const std::string& id, PermissionSet optional, const PermissionSet& granted) {
    auto& entry = entries_[id];
    entry.optional = std::move(optional);
    entry.granted = {};
    for (const auto& permission : granted.permissions)
      if (Contains(entry.optional.permissions, permission) && !Contains(entry.granted.permissions, permission)) entry.granted.permissions.push_back(permission);
    for (const auto& origin : granted.origins)
      if (Covered(entry.optional.origins, origin) && !Contains(entry.granted.origins, origin)) entry.granted.origins.push_back(origin);
  }
  void Forget(const std::string& id) { entries_.erase(id); }

  // Whether the extension holds the API permission, required or granted.
  bool Has(const std::string& id, const std::string& permission) const {
    auto found = entries_.find(id);
    if (found == entries_.end()) return false;
    return Contains(found->second.required.permissions, permission) || Contains(found->second.granted.permissions, permission);
  }
  // The sites it reaches, required and granted.
  std::vector<std::string> Hosts(const std::string& id) const {
    auto found = entries_.find(id);
    if (found == entries_.end()) return {};
    auto hosts = found->second.required.origins;
    for (const auto& host : found->second.granted.origins) if (!Contains(hosts, host)) hosts.push_back(host);
    return hosts;
  }
  // What the person granted it of what it may ask for.
  PermissionSet Granted(const std::string& id) const {
    auto found = entries_.find(id);
    return found == entries_.end() ? PermissionSet{} : found->second.granted;
  }
  // `getAll`: everything it holds.
  PermissionSet All(const std::string& id) const {
    auto found = entries_.find(id);
    if (found == entries_.end()) return {};
    auto all = found->second.required;
    Add(all, found->second.granted);
    return all;
  }
  // `contains`: whether it holds all of `asked`.
  bool Holds(const std::string& id, const PermissionSet& asked) const {
    for (const auto& permission : asked.permissions) if (!Has(id, permission)) return false;
    const auto hosts = Hosts(id);
    for (const auto& origin : asked.origins) if (!Covered(hosts, origin)) return false;
    return true;
  }
  // `request`: what of `asked` it does not hold yet, which the person is
  // asked for, or why it may not ask (it is not in the manifest).
  std::optional<std::string> Ask(const std::string& id, const PermissionSet& asked, PermissionSet& missing) const {
    auto found = entries_.find(id);
    if (found == entries_.end()) return std::string(kOnlyDeclared);
    return Check(found->second, asked, missing);
  }
  // The person granted `asked`: what of it is new.
  PermissionSet Grant(const std::string& id, const PermissionSet& asked) {
    auto found = entries_.find(id);
    if (found == entries_.end()) return {};
    return Add(found->second.granted, asked);
  }
  // `remove`: what of `asked` it held and no longer does, or why not (it is
  // required). Nothing is taken unless all of it may be.
  std::optional<std::string> Remove(const std::string& id, const PermissionSet& asked, PermissionSet& removed) {
    auto found = entries_.find(id);
    if (found == entries_.end()) return std::nullopt;
    auto& entry = found->second;
    for (const auto& permission : asked.permissions)
      if (Contains(entry.required.permissions, permission)) return std::string(kRequired);
    for (const auto& origin : asked.origins)
      for (const auto& host : entry.required.origins)
        if (PatternsOverlap(host, origin)) return std::string(kRequired);
    auto& granted = entry.granted;
    for (const auto& permission : asked.permissions) {
      auto it = std::find(granted.permissions.begin(), granted.permissions.end(), permission);
      if (it == granted.permissions.end()) continue;
      granted.permissions.erase(it);
      removed.permissions.push_back(permission);
    }
    for (auto it = granted.origins.begin(); it != granted.origins.end();) {
      const bool taken = std::any_of(asked.origins.begin(), asked.origins.end(), [&](const std::string& origin) { return PatternCovers(origin, *it); });
      if (!taken) { ++it; continue; }
      removed.origins.push_back(*it);
      it = granted.origins.erase(it);
    }
    return std::nullopt;
  }

 private:
  struct Entry { PermissionSet required, optional, granted; };
  static bool Contains(const std::vector<std::string>& list, const std::string& value) {
    return std::find(list.begin(), list.end(), value) != list.end();
  }
  // Whether one of `hosts` covers every page `origin` matches.
  static bool Covered(const std::vector<std::string>& hosts, const std::string& origin) {
    return std::any_of(hosts.begin(), hosts.end(), [&](const std::string& host) { return PatternCovers(host, origin); });
  }
  // Add what `more` has that `into` lacks; return that.
  static PermissionSet Add(PermissionSet& into, const PermissionSet& more) {
    PermissionSet added;
    for (const auto& permission : more.permissions)
      if (!Contains(into.permissions, permission)) { into.permissions.push_back(permission); added.permissions.push_back(permission); }
    for (const auto& origin : more.origins)
      if (!Contains(into.origins, origin)) { into.origins.push_back(origin); added.origins.push_back(origin); }
    return added;
  }
  // What of `asked` the entry does not hold, all of it among what the
  // manifest offers; otherwise why not.
  static std::optional<std::string> Check(const Entry& entry, const PermissionSet& asked, PermissionSet& missing) {
    for (const auto& permission : asked.permissions) {
      if (Contains(entry.required.permissions, permission) || Contains(entry.granted.permissions, permission)) continue;
      if (!Contains(entry.optional.permissions, permission)) return std::string(kOnlyDeclared);
      if (!Contains(missing.permissions, permission)) missing.permissions.push_back(permission);
    }
    for (const auto& origin : asked.origins) {
      if (!ParsePermissionPattern(origin)) return "Invalid value for origin pattern " + origin + ": Invalid scheme.";
      if (Covered(entry.required.origins, origin) || Covered(entry.granted.origins, origin)) continue;
      if (!Covered(entry.optional.origins, origin)) return std::string(kOnlyDeclared);
      if (!Contains(missing.origins, origin)) missing.origins.push_back(origin);
    }
    return std::nullopt;
  }
  std::map<std::string, Entry> entries_;
};

}  // namespace tilo
