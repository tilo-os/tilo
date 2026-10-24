#pragma once
// The port of a match pattern (`http://localhost:8080/*`), which Chrome
// reads apart from its host: a pattern naming no port, or `*`, matches a
// page on any port, and one naming a port only pages on it. Plain C++, so
// it is tested alone (tests/extension_permissions_test.cc) and both the
// pattern matching a page (match_pattern.h) and the patterns covering each
// other (extension_permissions.h) read a port alike.
#include <string>
#include <utility>

namespace tilo {

// A pattern's host part as host and port, the port empty where it names
// every port. An IPv6 address keeps the colons inside its brackets.
inline std::pair<std::string, std::string> SplitPatternHost(const std::string& host) {
  const auto colon = host.rfind(':');
  const auto bracket = host.rfind(']');
  if (colon == std::string::npos || (bracket != std::string::npos && colon < bracket)) return {host, std::string()};
  std::string port = host.substr(colon + 1);
  if (port == "*") port.clear();
  return {host.substr(0, colon), port};
}

// Whether `port` is one a pattern may name: digits, a port's few.
inline bool ValidPatternPort(const std::string& port) {
  if (port.size() > 5) return false;
  for (char c : port) if (c < '0' || c > '9') return false;
  return true;
}

// Whether a page of `scheme` on `port` (empty for its scheme's own) is on
// the port a pattern names (`want`, empty for every port).
inline bool PortMatches(const std::string& want, const std::string& scheme, const std::string& port) {
  if (want.empty()) return true;
  if (!port.empty()) return want == port;
  if (scheme == "http" || scheme == "ws") return want == "80";
  if (scheme == "https" || scheme == "wss") return want == "443";
  return false;
}

}  // namespace tilo
