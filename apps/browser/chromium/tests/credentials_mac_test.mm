#include "credentials_mac.h"
#include <cassert>

static int calls=0,adds=0;
static bool cancel_during_copy=false,cancelled=false;
static OSStatus add_status=errSecSuccess;
static OSStatus next_status=errSecSuccess;
static CFTypeRef output=nullptr;
static void Check(CFDictionaryRef query,bool account) {
  ++calls;
  assert(CFEqual(CFDictionaryGetValue(query,kSecClass),kSecClassGenericPassword));
  assert(CFEqual(CFDictionaryGetValue(query,kSecAttrService),
      CFSTR("dev.qos.browser.credentials.v1.test|https://example.com")));
  if(account)assert(CFEqual(CFDictionaryGetValue(query,kSecAttrAccount),CFSTR("account-one")));
}
static OSStatus Copy(CFDictionaryRef query,CFTypeRef* result) {
  Check(query,CFDictionaryContainsKey(query,kSecReturnData));
  if(!CFDictionaryContainsKey(query,kSecReturnData)) {
    assert(!CFDictionaryContainsKey(query,kSecAttrAccount));
    assert(CFEqual(CFDictionaryGetValue(query,kSecReturnAttributes),kCFBooleanTrue));
    assert(!CFDictionaryContainsKey(query,kSecValueData));
    int maximum=0;
    assert(CFNumberGetValue(static_cast<CFNumberRef>(CFDictionaryGetValue(query,kSecMatchLimit)),kCFNumberIntType,&maximum));
    assert(maximum==65); // one extra entry detects overflow instead of truncating

  }
  if(cancel_during_copy)cancelled=true;
  if(output && next_status==errSecSuccess)*result=CFRetain(output);
  return next_status;
}
static OSStatus Add(CFDictionaryRef query,CFTypeRef*) {
  ++adds;Check(query,true);assert(CFDictionaryContainsKey(query,kSecValueData));return add_status;
}
static OSStatus Update(CFDictionaryRef query,CFDictionaryRef values) {
  Check(query,true);assert(CFDictionaryContainsKey(values,kSecValueData));return next_status;
}
static OSStatus Delete(CFDictionaryRef query) {Check(query,true);return next_status;}
static qos::CredentialRequest Request(qos::CredentialOperation operation) {
  qos::CredentialRequest request;request.operation=operation;request.origin="https://example.com";
  request.account="account-one";request.username="synthetic-user";
  request.secret=qos::CredentialSecret("synthetic-secret");return request;
}
int main() {
  assert(qos::CredentialServiceForProfile("/isolated/profile")==qos::CredentialServiceForProfile("/isolated/profile"));
  assert(qos::CredentialServiceForProfile("/isolated/profile")!=qos::CredentialServiceForProfile("/other/profile"));
  assert(qos::CredentialServiceForProfile("/isolated/profile").find("/isolated")==std::string::npos);
  assert(qos::CredentialAccountId("same-user")==qos::CredentialAccountId("same-user"));
  assert(qos::CredentialAccountId("same-user")!=qos::CredentialAccountId("different-user"));
  assert(qos::CredentialAccountId("same-user").size()==64);

  qos::KeychainFunctions api{Copy,Add,Update,Delete};
  auto store=qos::MakeMacCredentialStore("dev.qos.browser.credentials.v1.test",api);
  assert(store);
  assert(!qos::MakeMacCredentialStore("unrelated.user.service",api));
  auto empty=CFArrayCreate(nullptr,nullptr,0,&kCFTypeArrayCallBacks);output=empty;
  assert(store->Execute(Request(qos::CredentialOperation::Save)).status==qos::CredentialStatus::Ok);
  add_status=errSecDuplicateItem;
  assert(store->Execute(Request(qos::CredentialOperation::Save)).status==qos::CredentialStatus::Exists);
  add_status=errSecSuccess;CFRelease(empty);output=nullptr;
  next_status=errSecItemNotFound;
  assert(store->Execute(Request(qos::CredentialOperation::Update)).status==qos::CredentialStatus::NotFound);
  next_status=errSecAuthFailed;
  assert(store->Execute(Request(qos::CredentialOperation::Fill)).status==qos::CredentialStatus::Denied);
  next_status=errSecSuccess;
  auto secret=CFDataCreate(nullptr,reinterpret_cast<const UInt8*>("synthetic-secret"),16);
  const void* read_keys[]={kSecAttrAccount,kSecAttrLabel,kSecValueData};
  const void* read_values[]={CFSTR("account-one"),CFSTR("stored-user"),secret};
  auto read_item=CFDictionaryCreate(nullptr,read_keys,read_values,3,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
  output=read_item;
  auto filled=store->Execute(Request(qos::CredentialOperation::Fill));
  assert(filled.status==qos::CredentialStatus::Ok && filled.secret.View()=="synthetic-secret");
  assert(filled.accounts.size()==1 && filled.accounts[0].username=="stored-user");
  CFRelease(read_item);CFRelease(secret);output=nullptr;
  assert(store->Execute(Request(qos::CredentialOperation::Delete)).status==qos::CredentialStatus::Ok);
  const auto before=calls;
  auto broad=Request(qos::CredentialOperation::Delete);broad.account.clear();
  assert(store->Execute(std::move(broad)).status==qos::CredentialStatus::Invalid);
  auto insecure=Request(qos::CredentialOperation::Fill);insecure.origin="http://example.com";
  assert(store->Execute(std::move(insecure)).status==qos::CredentialStatus::Invalid);
  assert(calls==before);
  next_status=errSecItemNotFound;
  auto listing=Request(qos::CredentialOperation::List);listing.account.clear();
  auto result=store->Execute(std::move(listing));
  assert(result.status==qos::CredentialStatus::Ok && result.accounts.empty());
  next_status=errSecSuccess;
  const void* keys[]={kSecAttrAccount,kSecAttrLabel};
  const void* values[]={CFSTR("account-one"),CFSTR("synthetic-user")};
  auto attributes=CFDictionaryCreate(nullptr,keys,values,2,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
  const void* items[]={attributes};
  auto array=CFArrayCreate(nullptr,items,1,&kCFTypeArrayCallBacks);output=array;
  auto accounts=store->Execute(Request(qos::CredentialOperation::List));
  assert(accounts.status==qos::CredentialStatus::Ok && accounts.accounts.size()==1);
  assert(accounts.accounts[0].id=="account-one" && accounts.accounts[0].username=="synthetic-user");
  assert(accounts.secret.View().empty());
  CFRelease(array);CFRelease(attributes);output=nullptr;

  // A full picker must never accept a new account it cannot subsequently show.
  auto full=CFArrayCreateMutable(nullptr,0,&kCFTypeArrayCallBacks);
  for(int i=0;i<64;++i) {
    auto id=CFStringCreateWithFormat(nullptr,nullptr,CFSTR("saved-%d"),i);
    const void* item_values[]={id,CFSTR("synthetic-user")};
    auto item=CFDictionaryCreate(nullptr,keys,item_values,2,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    CFArrayAppendValue(full,item);CFRelease(item);CFRelease(id);
  }
  output=full;
  const auto adds_before=adds;
  auto full_save=store->Execute(Request(qos::CredentialOperation::Save));
  assert(full_save.status==qos::CredentialStatus::Capacity);
  assert(adds==adds_before);
  auto full_list=store->Execute(Request(qos::CredentialOperation::List));
  assert(full_list.status==qos::CredentialStatus::Ok && full_list.accounts.size()==64);
  assert(store->Execute(Request(qos::CredentialOperation::Update)).status==qos::CredentialStatus::Ok);
  assert(store->Execute(Request(qos::CredentialOperation::Delete)).status==qos::CredentialStatus::Ok);
  CFArrayAppendValue(full,CFArrayGetValueAtIndex(full,0));
  auto overflow=store->Execute(Request(qos::CredentialOperation::List));
  assert(overflow.status==qos::CredentialStatus::Capacity && overflow.accounts.empty());
  CFArrayRemoveValueAtIndex(full,64);CFArrayRemoveValueAtIndex(full,63);
  assert(store->Execute(Request(qos::CredentialOperation::Save)).status==qos::CredentialStatus::Ok);
  assert(adds==adds_before+1);
  auto cancelled_save=Request(qos::CredentialOperation::Save);
  cancel_during_copy=true;
  cancelled_save.cancelled=[]{return cancelled;};
  const auto before_cancelled_add=adds;
  assert(store->Execute(std::move(cancelled_save)).status==qos::CredentialStatus::Cancelled);
  assert(adds==before_cancelled_add);
  CFRelease(full);output=nullptr;

}
