#include "credential_document.h"
#include <cassert>
#include <chrono>

using qos::CredentialDocument;
using qos::CredentialIdentity;
using qos::CredentialOperation;

static CredentialIdentity Document() {
  return {7, "main-frame", "https://example.com", "renderer-context-1", 1};
}

int main() {
  assert(qos::ValidCredentialCounter("1"));
  assert(qos::ValidCredentialCounter("9007199254740991"));
  for(const auto* invalid:{"", "0", "01", "-1", "1.0", "1x", "9007199254740992", "99999999999999999999"})
    assert(!qos::ValidCredentialCounter(invalid));
  const auto now = CredentialDocument::Clock::now();
  const auto document = Document();
  qos::CredentialHandshake handshake;
  handshake.Offer(document);
  auto prior = document;
  prior.generation++;
  assert(!handshake.Accept(prior));
  assert(!handshake.Accept(document)); // rejected reply burns challenge
  handshake.Offer(document);
  assert(handshake.Accept(document));
  assert(!handshake.Accept(document));
  handshake.Offer(document);
  handshake.Invalidate();
  assert(!handshake.Accept(document));
  handshake.Offer(document);
  auto child = document;
  child.frame = "child";
  assert(!handshake.Accept(child));
  CredentialDocument gate(false);
  assert(!gate.Begin(CredentialOperation::Fill, document, now));
  assert(gate.Commit(document, true, true, false));
  const auto before_hide = gate.Begin(CredentialOperation::Fill, document, now);
  assert(before_hide);
  gate.SetVisible(false);
  assert(!gate.Begin(CredentialOperation::Fill, document, now));
  assert(!gate.Take(*before_hide, CredentialOperation::Fill, document, now));
  gate.SetVisible(true);
  assert(!gate.Take(*before_hide, CredentialOperation::Fill, document, now));
  const auto after_show = gate.Begin(CredentialOperation::Fill, document, now);
  assert(after_show && *after_show != *before_hide);
  assert(gate.Take(*after_show, CredentialOperation::Fill, document, now));
  const auto fill = gate.Begin(CredentialOperation::Fill, document, now);
  assert(fill);
  auto wrong_origin = document;
  wrong_origin.origin = "https://example.com.evil.test";
  assert(!gate.Take(*fill, CredentialOperation::Fill, wrong_origin, now));
  assert(!gate.Take(*fill, CredentialOperation::Fill, document, now));

  for (auto changed : {0, 1, 2, 3, 4}) {
    assert(gate.Commit(document, true, true, false));
    const auto request = gate.Begin(CredentialOperation::Fill, document, now);
    assert(request);
    auto stale = document;
    if (changed == 0) stale.browser_id++;
    if (changed == 1) stale.frame = "child-frame";
    if (changed == 2) stale.origin = "https://example.com:8443";
    if (changed == 3) stale.context = "renderer-context-2";
    if (changed == 4) stale.generation++;
    assert(!gate.Take(*request, CredentialOperation::Fill, stale, now));
  }
  for (auto operation : {CredentialOperation::Capture, CredentialOperation::Save,
                         CredentialOperation::Update, CredentialOperation::Fill,
                         CredentialOperation::List, CredentialOperation::Delete}) {
    CredentialDocument private_gate(true);
    assert(!private_gate.Commit(document, true, true, false));
    assert(!private_gate.Begin(operation, document, now));
  }
  assert(!gate.Commit(document, false, true, false)); // subframe
  assert(!gate.Commit(document, true, false, false)); // invalid/unknown TLS
  assert(!gate.Commit(document, true, true, true)); // mixed content
  auto insecure = document;
  insecure.origin = "http://example.com";
  assert(!gate.Commit(insecure, true, true, false));
  assert(gate.Commit(document, true, true, false));
  const auto old = gate.Begin(CredentialOperation::Save, document, now);
  gate.Invalidate(); // navigation, renderer/context loss, close
  assert(!gate.Take(*old, CredentialOperation::Save, document, now));
  auto reload = document;
  reload.generation++;
  reload.context = "reload-context";
  assert(gate.Commit(reload, true, true, false));
  assert(!gate.Begin(CredentialOperation::Fill, document, now));
  const auto fresh = gate.Begin(CredentialOperation::Fill, reload, now);
  assert(fresh && *fresh != *old);
  assert(gate.Take(*fresh, CredentialOperation::Fill, reload, now));
  assert(!gate.Take(*fresh, CredentialOperation::Fill, reload, now)); // one shot
  const auto expired = gate.Begin(CredentialOperation::Fill, reload, now);
  assert(!gate.Take(*expired, CredentialOperation::Fill, reload,
                    now + std::chrono::seconds(30)));
  const auto wrong_action = gate.Begin(CredentialOperation::List, reload, now);
  assert(!gate.Take(*wrong_action, CredentialOperation::Delete, reload, now));
  assert(!gate.Take(*wrong_action, CredentialOperation::List, reload, now));
  for (size_t i = 0; i < CredentialDocument::kMaxPending; ++i)
    assert(gate.Begin(CredentialOperation::List, reload, now));
  assert(!gate.Begin(CredentialOperation::List, reload, now));
  // Expiration restores capacity without reusing request IDs.
  assert(gate.Begin(CredentialOperation::List, reload, now + std::chrono::seconds(31)));
}
