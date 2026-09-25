#pragma once
#include "include/cef_parser.h"
#include <cstdint>
#include <memory>
#include <string>

extern "C" {
void* qos_blocker_create(const uint8_t*,size_t);
void qos_blocker_destroy(void*);
char* qos_blocker_match(void*,const uint8_t*,size_t,const uint8_t*,size_t,const uint8_t*,size_t);
char* qos_blocker_cosmetics(void*,const uint8_t*,size_t,const uint8_t*,size_t,const uint8_t*,size_t);
void qos_blocker_string_free(char*);
}
namespace qos {
inline bool WebURL(const std::string& url) {
  CefURLParts parts;
  if(!CefParseURL(url,parts)) return false;
  auto scheme=CefString(&parts.scheme).ToString();
  return scheme=="http" || scheme=="https";
}
class Blocker {
 public:
  explicit Blocker(const std::string& rules):handle_(qos_blocker_create(Bytes(rules),rules.size())) {}
  ~Blocker() { qos_blocker_destroy(handle_); }
  Blocker(const Blocker&)=delete;
  Blocker& operator=(const Blocker&)=delete;
  bool Valid() const { return handle_!=nullptr; }
  CefRefPtr<CefDictionaryValue> Match(const std::string& url,const std::string& source,const std::string& kind) {
    return Parse(qos_blocker_match(handle_,Bytes(url),url.size(),Bytes(source),source.size(),Bytes(kind),kind.size()));
  }
  CefRefPtr<CefDictionaryValue> Cosmetics(const std::string& url,const std::string& classes,const std::string& ids) {
    return Parse(qos_blocker_cosmetics(handle_,Bytes(url),url.size(),Bytes(classes),classes.size(),Bytes(ids),ids.size()));
  }
 private:
  static const uint8_t* Bytes(const std::string& text) { return reinterpret_cast<const uint8_t*>(text.data()); }
  static CefRefPtr<CefDictionaryValue> Parse(char* text) {
    if(!text) return nullptr;
    auto value=CefParseJSON(text,JSON_PARSER_RFC);qos_blocker_string_free(text);
    return value && value->GetType()==VTYPE_DICTIONARY ? value->GetDictionary() : nullptr;
  }
  void* handle_;
};
}
