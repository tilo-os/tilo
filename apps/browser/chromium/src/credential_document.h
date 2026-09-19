#pragma once

#include <chrono>
#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

namespace qos {
inline bool ValidCredentialCounter(std::string_view value) {
  if(value.empty() || value.size()>16 || value.front()=='0')return false;
  uint64_t number=0;
  for(char c:value){if(c<'0' || c>'9')return false;number=number*10+static_cast<unsigned>(c-'0');}
  return number<=9007199254740991ULL;
}


// Host-owned identity, never accepted from page JavaScript or Rust IPC.
// origin must be canonicalized with qos::Origin from the committed CEF entry.
struct CredentialIdentity {
  int browser_id = 0;
  std::string frame;
  std::string origin;
  std::string context;
  uint64_t generation = 0;
  bool operator==(const CredentialIdentity&) const = default;
};

// A host-issued challenge binds one committed navigation to one renderer
// context response. The context token is assigned by the renderer, never JS.
class CredentialHandshake {
 public:
  void Offer(CredentialIdentity identity) { offered_ = std::move(identity); }
  void Invalidate() { offered_.reset(); }
  bool Accept(const CredentialIdentity& reply) {
    auto offered = std::exchange(offered_, std::nullopt);
    return offered && reply.browser_id == offered->browser_id &&
           reply.frame == offered->frame && reply.origin == offered->origin &&
           reply.generation == offered->generation &&
           !reply.context.empty() && reply.context.size() <= 128;
  }
 private:
  std::optional<CredentialIdentity> offered_;
};

enum class CredentialOperation { Capture, Save, Update, Fill, List, Delete };

// UI-thread state only. Workers receive immutable copies and return to the UI
// thread for Take before any DOM response. This class does not store secrets,
// establish TLS trust, call Keychain, or authorize a page-supplied request.
class CredentialDocument {
 public:
  using Clock = std::chrono::steady_clock;
  static constexpr size_t kMaxPending = 16;
  static constexpr auto kLifetime = std::chrono::seconds(30);

  explicit CredentialDocument(bool private_mode) : private_mode_(private_mode) {}

  // Called only after the committed main-frame/context handshake. Navigation
  // start, context loss, renderer loss and browser close must call Invalidate.
  bool Commit(const CredentialIdentity& identity, bool main_frame,
              bool certificate_valid, bool mixed_content) {
    Invalidate();
    if (private_mode_ || !main_frame || !certificate_valid || mixed_content ||
        identity.browser_id <= 0 || identity.generation == 0 ||
        identity.frame.empty() || identity.frame.size() > 256 ||
        identity.context.empty() || identity.context.size() > 128 ||
        !identity.origin.starts_with("https://") || identity.origin.size() <= 8 ||
        identity.origin.size() > 8192)
      return false;
    current_ = identity;
    return true;
  }

  void SetVisible(bool visible) {
    visible_ = visible;
    if (!visible) pending_.clear();
  }

  void Invalidate() {
    current_.reset();
    pending_.clear();
  }

  // Chrome consent and operation-specific form/account checks belong in the
  // broker before Begin. The returned ID is opaque, bounded, and never reused.
  std::optional<uint64_t> Begin(CredentialOperation operation,
                                const CredentialIdentity& identity,
                                Clock::time_point now) {
    Expire(now);
    if (private_mode_ || !visible_ || !current_ || *current_ != identity ||
        pending_.size() >= kMaxPending || next_id_ > kMaxExactJsonInteger)
      return std::nullopt;
    const uint64_t id = next_id_++;
    pending_.emplace(id, Pending{operation, identity, now + kLifetime});
    return id;
  }

  // Consume even a mismatched completion: retry cannot revive stale consent.
  bool Take(uint64_t id, CredentialOperation operation,
            const CredentialIdentity& identity, Clock::time_point now) {
    auto found = pending_.find(id);
    if (found == pending_.end()) return false;
    const auto pending = std::move(found->second);
    pending_.erase(found);
    return !private_mode_ && visible_ && current_ && *current_ == identity &&
           pending.identity == identity && pending.operation == operation &&
           now < pending.expires;
  }

 private:
  struct Pending {
    CredentialOperation operation;
    CredentialIdentity identity;
    Clock::time_point expires;
  };
  void Expire(Clock::time_point now) {
    for (auto it = pending_.begin(); it != pending_.end();) {
      if (now >= it->second.expires) it = pending_.erase(it);
      else ++it;
    }
  }
  static constexpr uint64_t kMaxExactJsonInteger = (uint64_t{1} << 53) - 1;
  const bool private_mode_;
  bool visible_ = true;
  uint64_t next_id_ = 1;
  std::optional<CredentialIdentity> current_;
  std::map<uint64_t, Pending> pending_;
};
}  // namespace qos
