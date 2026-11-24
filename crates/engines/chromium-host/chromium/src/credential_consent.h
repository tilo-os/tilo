#pragma once
#include "credential_document.h"
#include <optional>
#include <string>
#include <string_view>
#include <vector>
namespace tilo {
// Move-only owned buffer. OS/CEF copies require their own lifetime management;
// wiping this allocation is not a claim of perfect process-memory erasure.
class CredentialSecret {
 public:
  CredentialSecret()=default;
  explicit CredentialSecret(std::string_view value):bytes_(value.begin(),value.end()) {}
  CredentialSecret(const CredentialSecret&)=delete;
  CredentialSecret& operator=(const CredentialSecret&)=delete;
  CredentialSecret(CredentialSecret&& other) noexcept:bytes_(std::move(other.bytes_)) {}
  CredentialSecret& operator=(CredentialSecret&& other) noexcept {
    if(this!=&other){Clear();bytes_=std::move(other.bytes_);}return *this;
  }
  ~CredentialSecret(){Clear();}
  std::string_view View() const {return {bytes_.data(),bytes_.size()};}
 private:
  void Clear(){volatile char* data=bytes_.data();for(size_t i=0;i<bytes_.size();++i)data[i]=0;bytes_.clear();}
  std::vector<char> bytes_;
};
struct CapturedCredential {std::string form, username;CredentialSecret secret;};
// One pending capture per tab, until a take hands it to the vault. Values remain native until
// that take. The owner must Clear on navigation/cancel and at expiry.
class CredentialConsent {
 public:
  using Clock=CredentialDocument::Clock;
  bool Offer(std::string id,CredentialIdentity identity,std::string form,std::string username,
             CredentialSecret secret,Clock::time_point now) {
    Clear();
    if(!ValidCredentialCounter(id) || !ValidCredentialCounter(form) || username.size()>1024 || secret.View().empty() || secret.View().size()>4096)return false;
    pending_.emplace(Pending{std::move(id),std::move(identity),{std::move(form),std::move(username),std::move(secret)},now+std::chrono::seconds(30)});
    return true;
  }
  std::optional<CapturedCredential> Take(const std::string& id,const CredentialIdentity& identity,Clock::time_point now) {
    auto pending=std::exchange(pending_,std::nullopt);
    if(!pending || pending->id!=id || pending->identity!=identity || now>=pending->expires)return {};
    return std::move(pending->captured);
  }
  void Clear(){pending_.reset();}
  void Expire(Clock::time_point now){if(pending_ && now>=pending_->expires)Clear();}
 private:
  struct Pending {std::string id;CredentialIdentity identity;CapturedCredential captured;Clock::time_point expires;};
  std::optional<Pending> pending_;
};
}
