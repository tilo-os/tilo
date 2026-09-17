#pragma once
#include <cstddef>
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
    const auto key=std::make_pair(owner,url);auto [it,inserted]=methods_.emplace(key,method);
    if(!inserted)it->second.clear();
    if(methods_.size()>64)methods_.erase(methods_.begin());
  }
  bool TakeRetryable(int owner,const std::string& original_url,const std::string& download_url) {
    auto found=methods_.find(std::make_pair(owner,original_url));
    if(found==methods_.end() && original_url!=download_url)found=methods_.find(std::make_pair(owner,download_url));
    if(found==methods_.end())return false;
    const bool retryable=found->second=="GET" && Network(download_url);methods_.erase(found);return retryable;
  }
 private:
  static bool Network(const std::string& url){return url.rfind("https://",0)==0 || url.rfind("http://",0)==0;}
  std::map<std::pair<int,std::string>,std::string> methods_;
};
}
