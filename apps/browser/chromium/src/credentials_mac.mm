#include "credentials_mac.h"
#include <algorithm>
#include <CommonCrypto/CommonDigest.h>

namespace qos {
namespace {
std::string IdentityHash(std::string_view value) {
  if(value.size()>8192)return {};
  unsigned char bytes[CC_SHA256_DIGEST_LENGTH];
  CC_SHA256(value.data(),static_cast<CC_LONG>(value.size()),bytes);
  constexpr char hex[]="0123456789abcdef";std::string result;
  for(auto byte:bytes){result.push_back(hex[byte>>4]);result.push_back(hex[byte&15]);}
  return result;
}
struct Owned {
  CFTypeRef value=nullptr;
  explicit Owned(CFTypeRef v):value(v){}
  ~Owned(){if(value)CFRelease(value);}
  Owned(const Owned&)=delete;
};
CFStringRef String(const std::string& value) {
  return CFStringCreateWithBytes(nullptr,reinterpret_cast<const UInt8*>(value.data()),
                                value.size(),kCFStringEncodingUTF8,false);
}
std::optional<std::string> Text(CFTypeRef value,size_t limit) {
  if(!value || CFGetTypeID(value)!=CFStringGetTypeID())return {};
  auto text=static_cast<CFStringRef>(value);
  if(CFStringGetLength(text)>static_cast<CFIndex>(limit))return {};
  std::vector<char> bytes(limit+1);
  if(!CFStringGetCString(text,bytes.data(),bytes.size(),kCFStringEncodingUTF8))return {};
  return std::string(bytes.data());
}
CredentialStatus Status(OSStatus status) {
  if(status==errSecSuccess)return CredentialStatus::Ok;
  if(status==errSecItemNotFound)return CredentialStatus::NotFound;
  if(status==errSecDuplicateItem)return CredentialStatus::Exists;
  if(status==errSecAuthFailed || status==errSecInteractionNotAllowed || status==errSecUserCanceled)
    return CredentialStatus::Denied;
  return CredentialStatus::Unavailable;
}
class MacStore final : public CredentialStore {
 public:
  MacStore(std::string service,KeychainFunctions api):service_(std::move(service)),api_(api){}
  CredentialResult Execute(CredentialRequest request) override {
    CredentialResult result;result.status=CredentialStatus::Invalid;
    if(request.cancelled && request.cancelled()){result.status=CredentialStatus::Cancelled;return result;}
    const bool list=request.operation==CredentialOperation::List;
    // Canonicalization and authenticated-document consent are broker duties.
    // Reject non-origin shapes here too; never turn an empty account into a
    // broad update/delete/read query.
    if(!request.origin.starts_with("https://") || request.origin.size()<=8 || request.origin.size()>8192 ||
       request.origin.find_first_of("/?#@|\\",8)!=std::string::npos ||
       std::any_of(request.origin.begin(),request.origin.end(),[](unsigned char c){return c<=32 || c==127;}) ||
       (!list && request.account.empty()) || request.account.size()>128 || request.username.size()>1024 ||
       request.secret.View().size()>4096 || request.operation==CredentialOperation::Capture)return result;
    if(request.operation==CredentialOperation::Save) {
      // The profile's serial worker owns both this attributes-only preflight
      // and Add. Never save an account beyond the bounded picker capacity.
      CredentialRequest listing;listing.operation=CredentialOperation::List;listing.origin=request.origin;
      auto existing=Execute(std::move(listing));
      if(request.cancelled && request.cancelled()){result.status=CredentialStatus::Cancelled;return result;}
      if(existing.status!=CredentialStatus::Ok){result.status=existing.status;return result;}
      if(std::any_of(existing.accounts.begin(),existing.accounts.end(),[&](const auto& item){return item.id==request.account;})) {
        result.status=CredentialStatus::Exists;return result;
      }
      if(existing.accounts.size()>=64){result.status=CredentialStatus::Capacity;return result;}
    }
    auto query=CFDictionaryCreateMutable(nullptr,0,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    Owned query_owner(query);
    Owned service(String(service_+"|"+request.origin)),account(String(request.account)),username(String(request.username));
    if(!service.value || !account.value || !username.value)return result;
    CFDictionarySetValue(query,kSecClass,kSecClassGenericPassword);
    CFDictionarySetValue(query,kSecAttrService,service.value);
    if(!list)CFDictionarySetValue(query,kSecAttrAccount,account.value);
    if(request.operation==CredentialOperation::Delete){result.status=Status(api_.remove(query));return result;}
    if(request.operation==CredentialOperation::Save || request.operation==CredentialOperation::Update) {
      if(request.secret.View().empty())return result;
      Owned data(CFDataCreate(nullptr,reinterpret_cast<const UInt8*>(request.secret.View().data()),request.secret.View().size()));
      if(request.operation==CredentialOperation::Save) {
        CFDictionarySetValue(query,kSecAttrLabel,username.value);
        CFDictionarySetValue(query,kSecValueData,data.value);
        result.status=Status(api_.add(query,nullptr));
      } else {
        auto changes=CFDictionaryCreateMutable(nullptr,0,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
        Owned changes_owner(changes);
        CFDictionarySetValue(changes,kSecAttrLabel,username.value);CFDictionarySetValue(changes,kSecValueData,data.value);
        result.status=Status(api_.update(query,changes));
      }
      return result;
    }
    if(list) {
      CFDictionarySetValue(query,kSecReturnAttributes,kCFBooleanTrue);
      int maximum=65;Owned limit(CFNumberCreate(nullptr,kCFNumberIntType,&maximum));
      CFDictionarySetValue(query,kSecMatchLimit,limit.value);
    } else {
      CFDictionarySetValue(query,kSecReturnData,kCFBooleanTrue);
      CFDictionarySetValue(query,kSecReturnAttributes,kCFBooleanTrue);
      CFDictionarySetValue(query,kSecMatchLimit,kSecMatchLimitOne);
    }
    CFTypeRef raw=nullptr;const auto status=api_.copy(query,&raw);Owned raw_owner(raw);
    result.status=Status(status);
    if(list && status==errSecItemNotFound){result.status=CredentialStatus::Ok;return result;}
    if(status!=errSecSuccess)return result;
    result.status=CredentialStatus::Invalid;
    if(!raw)return result;
    if(!list) {
      if(CFGetTypeID(raw)!=CFDictionaryGetTypeID())return result;
      auto attributes=static_cast<CFDictionaryRef>(raw);
      auto id=Text(CFDictionaryGetValue(attributes,kSecAttrAccount),128);
      auto label=Text(CFDictionaryGetValue(attributes,kSecAttrLabel),1024);
      auto stored=CFDictionaryGetValue(attributes,kSecValueData);
      if(!id || *id!=request.account || !label || !stored || CFGetTypeID(stored)!=CFDataGetTypeID())return result;
      auto data=static_cast<CFDataRef>(stored);const auto length=CFDataGetLength(data);
      if(length<=0 || length>4096)return result;
      result.secret=CredentialSecret({reinterpret_cast<const char*>(CFDataGetBytePtr(data)),static_cast<size_t>(length)});
      result.accounts.push_back({*id,*label});
    } else {
      if(CFGetTypeID(raw)!=CFArrayGetTypeID())return result;
      auto entries=static_cast<CFArrayRef>(raw);
      if(CFArrayGetCount(entries)>64){result.status=CredentialStatus::Capacity;return result;}
      for(CFIndex i=0;i<CFArrayGetCount(entries);++i) {
        auto entry=CFArrayGetValueAtIndex(entries,i);
        if(!entry || CFGetTypeID(entry)!=CFDictionaryGetTypeID()){result.accounts.clear();return result;}
        auto attributes=static_cast<CFDictionaryRef>(entry);
        auto id=Text(CFDictionaryGetValue(attributes,kSecAttrAccount),128);
        auto label=Text(CFDictionaryGetValue(attributes,kSecAttrLabel),1024);
        if(!id || id->empty() || !label){result.accounts.clear();return result;}
        result.accounts.push_back({*id,*label});
      }
    }
    result.status=CredentialStatus::Ok;return result;
  }
 private: std::string service_;KeychainFunctions api_;
};
}
std::string CredentialServiceForProfile(std::string_view canonical_profile) {
  if(canonical_profile.empty())return {};
  auto hash=IdentityHash(canonical_profile);
  return hash.empty()?std::string():"dev.qos.browser.credentials.v1."+hash;
}
std::string CredentialAccountId(std::string_view username) {return IdentityHash(username);}
std::unique_ptr<CredentialStore> MakeMacCredentialStore(std::string service,KeychainFunctions api) {
  constexpr std::string_view prefix="dev.qos.browser.credentials.v1.";
  if(!service.starts_with(prefix) || service.size()<=prefix.size() || service.size()>256 ||
     !std::all_of(service.begin(),service.end(),[](unsigned char c){return (c>='a'&&c<='z') ||
         (c>='A'&&c<='Z') || (c>='0'&&c<='9') || c=='.' || c=='-';}) ||
     !api.copy || !api.add || !api.update || !api.remove)return {};
  return std::make_unique<MacStore>(std::move(service),api);
}
}
