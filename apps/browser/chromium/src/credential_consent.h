#pragma once
#include "credential_worker.h"
namespace qos {
struct CapturedCredential {std::string form, username;CredentialSecret secret;};
// One pending Save/Update consent per tab. Values remain native, never in the
// Rust metadata event. The owner must Clear on navigation/cancel and at expiry.
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
