#pragma once
#include <set>
#include <string>
#include <vector>
namespace tilo {
// The fields each credential action takes. A password crosses the host's own protocol only to
// fill a form with what the workspace's vault handed over for the page's site; what a capture
// read leaves only in a take's reply, for the vault to keep.
inline bool CredentialFieldsValid(const std::string& action,const std::vector<std::string>& fields) {
  std::set<std::string> required={"op","request_id","action","origin","document"};
  if(action=="capture")required.insert("form");
  else if(action=="take")required.insert("capture");
  else if(action=="fill"){required.insert("form");required.insert("username");required.insert("password");}
  else if(action=="fill_address"){required.insert("form");required.insert("profile");}
  else if(action!="status" && action!="cancel" && action!="discover" && action!="discover_addresses")return false;
  std::set<std::string> actual(fields.begin(),fields.end());
  if(actual.size()!=fields.size())return false;
  actual.erase("tab");return actual==required;
}
}
