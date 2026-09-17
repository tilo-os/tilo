#pragma once
#include <set>
#include <string>
#include <vector>
namespace qos {
// Public chrome/native IPC never accepts a password or a fill username. Those
// values travel exclusively through renderer messages and Keychain results.
inline bool CredentialFieldsValid(const std::string& action,const std::vector<std::string>& fields) {
  std::set<std::string> required={"op","request_id","action","origin","document"};
  if(action=="capture")required.insert("form");
  else if(action=="save")required.insert("capture");
  else if(action=="update"){required.insert("capture");required.insert("account");}
  else if(action=="fill"){required.insert("form");required.insert("account");}
  else if(action=="delete")required.insert("account");
  else if(action=="fill_address"){required.insert("form");required.insert("profile");}
  else if(action!="status" && action!="cancel" && action!="discover" && action!="discover_addresses" && action!="list")return false;
  std::set<std::string> actual(fields.begin(),fields.end());
  if(actual.size()!=fields.size())return false;
  actual.erase("tab");return actual==required;
}
}
