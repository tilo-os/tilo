#include "credential_commands.h"
#include <cassert>
int main() {
  const std::vector<std::string> base={"op","request_id","action","origin","document","tab"};
  auto keys=base;
  assert(qos::CredentialFieldsValid("list",keys));
  assert(!qos::CredentialFieldsValid("save",keys));
  keys.push_back("capture");assert(qos::CredentialFieldsValid("save",keys));
  assert(!qos::CredentialFieldsValid("update",keys));
  keys.push_back("account");assert(qos::CredentialFieldsValid("update",keys));
  keys.push_back("password");assert(!qos::CredentialFieldsValid("update",keys));
  keys=base;keys.push_back("form");keys.push_back("account");
  assert(qos::CredentialFieldsValid("fill",keys));
  keys.push_back("username");assert(!qos::CredentialFieldsValid("fill",keys));
  keys=base;keys.push_back("form");keys.push_back("profile");
  assert(qos::CredentialFieldsValid("fill_address",keys));
  assert(!qos::CredentialFieldsValid("unknown",base));
  keys=base;keys.erase(keys.begin());assert(!qos::CredentialFieldsValid("status",keys));
}
