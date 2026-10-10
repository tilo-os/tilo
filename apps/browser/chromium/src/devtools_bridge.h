#pragma once
// Browser-level DevTools for extension management.
//
// CEF's Chrome runtime exposes no extension API of its own, but Chromium's
// `Extensions` DevTools domain loads and unloads unpacked extensions without a
// restart. Chromium serves it on `--remote-debugging-pipe` (fd 3 reads
// commands, fd 4 writes replies, messages NUL-terminated). The host owns both
// pipe ends and offers only the commands extension management needs, so no
// port is ever opened and the Rust application never receives a general
// DevTools session.
#include <cstdint>
#include <map>
#include <mutex>
#include <optional>
#include <set>
#include <string>
namespace tilo {
inline bool DevToolsMethodAllowed(const std::string& method) {
  static const std::set<std::string> allowed{
    "Extensions.loadUnpacked","Extensions.uninstall",
    // Extension service workers, for the toolbar's action state.
    "Target.getTargets","Target.attachToTarget","Target.detachFromTarget","Runtime.evaluate",
    "Browser.getVersion"};
  return allowed.count(method)>0;
}
inline constexpr size_t kMaxDevToolsMessage=1024*1024;
inline constexpr size_t kMaxPendingDevTools=64;
struct DevToolsOwner {int tab=0;int request_id=0;};
// Host-wide CDP ids to the tab request awaiting each reply.
class DevToolsPending {
 public:
  std::optional<int> Add(int tab,int request_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    if(pending_.size()>=kMaxPendingDevTools) return std::nullopt;
    next_=next_%2147483647+1;
    pending_[next_]={tab,request_id};
    return next_;
  }
  std::optional<DevToolsOwner> Take(int id) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto found=pending_.find(id);
    if(found==pending_.end()) return std::nullopt;
    auto owner=found->second;pending_.erase(found);return owner;
  }
  void Forget(int id) {std::lock_guard<std::mutex> lock(mutex_);pending_.erase(id);}
 private:
  std::mutex mutex_;int next_=0;std::map<int,DevToolsOwner> pending_;
};
// Split NUL-terminated messages out of `buffer`. False when one is too large.
inline bool TakeDevToolsMessages(std::string& buffer,std::string* out,size_t max_out,size_t* count) {
  *count=0;
  for(;;) {
    auto end=buffer.find('\0');
    if(end==std::string::npos) return buffer.size()<=kMaxDevToolsMessage;
    if(end>kMaxDevToolsMessage) return false;
    if(*count<max_out) out[(*count)++]=buffer.substr(0,end);
    buffer.erase(0,end+1);
    if(*count==max_out) return true;
  }
}
}
