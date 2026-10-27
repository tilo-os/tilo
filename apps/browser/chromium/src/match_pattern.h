#pragma once
// Chrome's match patterns (`<all_urls>`, `*://*.example.com/*`), which say
// which pages an extension reaches.
#include <string>
#include "include/cef_parser.h"
#include "match_host.h"

namespace tilo {

// `*` matches any run of characters.
inline bool GlobMatch(const std::string& pattern, const std::string& text) {
  size_t p = 0, t = 0, star = std::string::npos, mark = 0;
  while (t < text.size()) {
    if (p < pattern.size() && pattern[p] == '*') { star = p++; mark = t; }
    else if (p < pattern.size() && pattern[p] == text[t]) { ++p; ++t; }
    else if (star != std::string::npos) { p = star + 1; t = ++mark; }
    else return false;
  }
  while (p < pattern.size() && pattern[p] == '*') ++p;
  return p == pattern.size();
}
// Chrome's match patterns (`<all_urls>`, `*://*.example.com/*`), for access to
// a page.
inline bool MatchPattern(const std::string& pattern, const std::string& url) {
  CefURLParts parts;
  if (!CefParseURL(url, parts)) return false;
  const std::string scheme = CefString(&parts.scheme).ToString();
  const std::string host = CefString(&parts.host).ToString();
  std::string path = CefString(&parts.path).ToString();
  const std::string query = CefString(&parts.query).ToString();
  if (!query.empty()) path += "?" + query;
  if (pattern == "<all_urls>") return scheme == "http" || scheme == "https" || scheme == "file" || scheme == "ws" || scheme == "wss";
  const auto separator = pattern.find("://");
  if (separator == std::string::npos) return false;
  const std::string want_scheme = pattern.substr(0, separator);
  const auto slash = pattern.find('/', separator + 3);
  // A pattern naming a port (`http://localhost:8080/*`) matches pages on
  // that port alone; one naming none, on every port.
  const auto [want_host, want_port] = SplitPatternHost(pattern.substr(separator + 3, slash == std::string::npos ? std::string::npos : slash - separator - 3));
  const std::string want_path = slash == std::string::npos ? "/*" : pattern.substr(slash);
  if (want_scheme == "*" ? scheme != "http" && scheme != "https" : want_scheme != scheme) return false;
  if (!PortMatches(want_port, scheme, CefString(&parts.port).ToString())) return false;
  if (want_host != "*" && scheme != "file") {
    if (want_host.rfind("*.", 0) == 0) {
      const std::string base = want_host.substr(2);
      if (host != base && !(host.size() > base.size() && host.compare(host.size() - base.size() - 1, std::string::npos, "." + base) == 0)) return false;
    } else if (want_host != host) {
      return false;
    }
  }
  return GlobMatch(want_path, path.empty() ? "/" : path);
}

}  // namespace tilo
