#pragma once
#include "credential_worker.h"
#include <Security/Security.h>
namespace qos {
std::string CredentialServiceForProfile(std::string_view canonical_profile);
std::string CredentialAccountId(std::string_view username);
struct KeychainFunctions {
  decltype(&SecItemCopyMatching) copy=&SecItemCopyMatching;
  decltype(&SecItemAdd) add=&SecItemAdd;
  decltype(&SecItemUpdate) update=&SecItemUpdate;
  decltype(&SecItemDelete) remove=&SecItemDelete;
};
// Call Execute through CredentialWorker only. Namespace is a stable profile
// identifier under the browser prefix; each operation also scopes exact origin.
std::unique_ptr<CredentialStore> MakeMacCredentialStore(std::string service,
    KeychainFunctions functions={});
}
