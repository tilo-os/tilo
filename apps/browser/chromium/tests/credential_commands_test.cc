#include "credential_commands.h"
#include <cassert>
int main() {
  const std::vector<std::string> base={"op","request_id","action","origin","document","tab"};
  auto keys=base;
  assert(tilo::CredentialFieldsValid("list",keys));
  assert(!tilo::CredentialFieldsValid("save",keys));
  keys.push_back("capture");assert(tilo::CredentialFieldsValid("save",keys));
  assert(!tilo::CredentialFieldsValid("update",keys));
  keys.push_back("account");assert(tilo::CredentialFieldsValid("update",keys));
  keys.push_back("password");assert(!tilo::CredentialFieldsValid("update",keys));
  keys=base;keys.push_back("form");keys.push_back("account");
  assert(tilo::CredentialFieldsValid("fill",keys));
  keys.push_back("username");assert(!tilo::CredentialFieldsValid("fill",keys));
  keys=base;keys.push_back("form");keys.push_back("profile");
  assert(tilo::CredentialFieldsValid("fill_address",keys));
  assert(!tilo::CredentialFieldsValid("unknown",base));
  keys=base;keys.erase(keys.begin());assert(!tilo::CredentialFieldsValid("status",keys));
}
