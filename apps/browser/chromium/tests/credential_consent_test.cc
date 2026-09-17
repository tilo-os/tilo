#include "credential_consent.h"
#include <cassert>
int main() {
  qos::CredentialConsent consent;
  qos::CredentialIdentity identity{1,"main","https://example.com","context",1};
  auto now=qos::CredentialDocument::Clock::now();
  assert(!consent.Take("1",identity,now));
  assert(consent.Offer("1",identity,"7","user",qos::CredentialSecret("secret"),now));
  auto different=identity;different.origin="https://other.example";
  assert(!consent.Take("1",different,now));
  assert(!consent.Take("1",identity,now));
  assert(consent.Offer("2",identity,"7","user",qos::CredentialSecret("secret"),now));
  assert(!consent.Take("2",identity,now+std::chrono::seconds(30)));
  assert(consent.Offer("3",identity,"7","user",qos::CredentialSecret("secret"),now));
  consent.Clear();assert(!consent.Take("3",identity,now));
  assert(consent.Offer("4",identity,"7","user",qos::CredentialSecret("secret"),now));
  auto chosen=consent.Take("4",identity,now);
  assert(chosen && chosen->form=="7" && chosen->username=="user" && chosen->secret.View()=="secret");
  assert(!consent.Take("4",identity,now));
  assert(consent.Offer("5",identity,"7","user",qos::CredentialSecret("secret"),now));
  auto reload=identity;reload.generation++;
  assert(!consent.Take("5",reload,now));
  assert(!consent.Offer("0",identity,"7","user",qos::CredentialSecret("secret"),now));
}
