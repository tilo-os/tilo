#pragma once
#include <cstddef>
#include <deque>
#include <map>
#include <set>
#include <string>
#include <utility>
#include <vector>

namespace qos {
class DownloadSlots {
 public:
  static constexpr size_t kLimit=32;
  bool Admit(int id) {
    if(ids_.count(id))return true;
    if(ids_.size()>=kLimit)return false;
    ids_.insert(id);return true;
  }
  bool Stage(int id,int owner) {
    if(!Admit(id))return false;
    early_.try_emplace(id,owner);return true;
  }
  void Promote(int id){early_.erase(id);}
  std::vector<int> EarlyForOwner(int owner) const {
    std::vector<int> ids;for(const auto& [id,found]:early_)if(found==owner)ids.push_back(id);return ids;
  }
  void Settle(int id){early_.erase(id);ids_.erase(id);}
  bool Contains(int id) const{return ids_.count(id);}
  size_t Size() const{return ids_.size();}
 private:
  std::set<int> ids_;
  std::map<int,int> early_;
};

// CEF reports request methods before it assigns a download ID. A repeated
// owner+URL is deliberately ambiguous so concurrent GET/POST downloads can
// never make a POST body appear safely replayable.
class DownloadProvenance {
 public:
  void Record(int owner,const std::string& url,const std::string& method) {
    if(disabled_)return;
    if(pending_>=64){methods_.clear();pending_=0;disabled_=true;return;}
    auto& requests=methods_[std::make_pair(owner,url)];
    const bool ambiguous=!requests.empty();
    if(ambiguous)for(auto& request:requests)request.ambiguous=true;
    requests.push_back({method,ambiguous});++pending_;
  }
  bool TakeRetryable(int owner,const std::string& original_url,const std::string& download_url) {
    if(disabled_)return false;
    auto found=methods_.find(std::make_pair(owner,original_url));
    if(found==methods_.end() && original_url!=download_url)found=methods_.find(std::make_pair(owner,download_url));
    if(found==methods_.end())return false;
    auto request=found->second.front();found->second.pop_front();--pending_;
    if(found->second.empty())methods_.erase(found);
    return !request.ambiguous && request.method=="GET" && Network(download_url);
  }
 private:
  struct Request {std::string method;bool ambiguous=false;};
  static bool Network(const std::string& url){return url.rfind("https://",0)==0 || url.rfind("http://",0)==0;}
  std::map<std::pair<int,std::string>,std::deque<Request>> methods_;
  size_t pending_=0;bool disabled_=false;
};

// A busy visible owner reserves the detached slot it may need. Once detached,
// that reservation remains until OnBeforeClose confirms physical destruction,
// even when its last transfer has already settled.
class DownloadOwnerReservations {
 public:
  static constexpr size_t kLimit=32;
  bool CanAdmit(int owner) const {return IsBusy(owner) || IsDetached(owner) || Size()<kLimit;}
  void SetBusy(int owner,bool busy){if(busy)busy_.insert(owner);else busy_.erase(owner);}
  bool Detach(int owner){if(!CanAdmit(owner))return false;detached_.insert(owner);return true;}
  void Destroy(int owner){busy_.erase(owner);detached_.erase(owner);}
  bool IsBusy(int owner) const{return busy_.count(owner);}
  bool IsDetached(int owner) const{return detached_.count(owner);}
  size_t Size() const {
    size_t size=detached_.size();for(int owner:busy_)if(!detached_.count(owner))++size;return size;
  }
 private:
  std::set<int> busy_,detached_;
};

inline bool ShouldCloseDetached(bool detached,bool busy){return detached && !busy;}
}
