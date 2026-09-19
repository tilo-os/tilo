#pragma once
#include "include/cef_parser.h"
#include <string>
namespace qos {
// Match Chromium's serialized HTTP(S) origin, excluding credentials/path.
inline std::string Origin(const std::string& url) {
  CefURLParts parts;if(url.size()>8192 || !CefParseURL(url,parts)) return {};
  const auto scheme=CefString(&parts.scheme).ToString(),host=CefString(&parts.host).ToString(),port=CefString(&parts.port).ToString();
  if((scheme!="http" && scheme!="https") || host.empty()) return {};
  return scheme+"://"+host+((port.empty() || (scheme=="https" && port=="443") || (scheme=="http" && port=="80"))?"":":"+port);
}
}
