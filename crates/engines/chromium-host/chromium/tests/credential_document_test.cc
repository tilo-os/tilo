#include "credential_document.h"
#include <cassert>
#include <chrono>

using tilo::CredentialDocument;
using tilo::CredentialIdentity;
using tilo::CredentialOperation;

static CredentialIdentity Document() {
  return {7, "main-frame", "https://example.com", "renderer-context-1", 1};
}

int main() {
  assert(tilo::ValidCredentialCounter("1"));
  assert(tilo::ValidCredentialCounter("9007199254740991"));
  for(const auto* invalid:{"", "0", "01", "-1", "1.0", "1x", "9007199254740992", "99999999999999999999"})
    assert(!tilo::ValidCredentialCounter(invalid));
  const auto now = CredentialDocument::Clock::now();
  const auto document = Document();
  tilo::CredentialHandshake handshake;
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
  CredentialDocument check(false);
  assert(!check.Begin(CredentialOperation::Fill, document, now));
  assert(check.Commit(document, true, true, false));
  const auto before_hide = check.Begin(CredentialOperation::Fill, document, now);
  assert(before_hide);
  check.SetVisible(false);
  assert(!check.Begin(CredentialOperation::Fill, document, now));
  assert(!check.Take(*before_hide, CredentialOperation::Fill, document, now));
  check.SetVisible(true);
  assert(!check.Take(*before_hide, CredentialOperation::Fill, document, now));
  const auto after_show = check.Begin(CredentialOperation::Fill, document, now);
  assert(after_show && *after_show != *before_hide);
  assert(check.Take(*after_show, CredentialOperation::Fill, document, now));
  const auto fill = check.Begin(CredentialOperation::Fill, document, now);
  assert(fill);
  auto wrong_origin = document;
  wrong_origin.origin = "https://example.com.evil.test";
  assert(!check.Take(*fill, CredentialOperation::Fill, wrong_origin, now));
  assert(!check.Take(*fill, CredentialOperation::Fill, document, now));

  for (auto changed : {0, 1, 2, 3, 4}) {
    assert(check.Commit(document, true, true, false));
    const auto request = check.Begin(CredentialOperation::Fill, document, now);
    assert(request);
    auto stale = document;
    if (changed == 0) stale.browser_id++;
    if (changed == 1) stale.frame = "child-frame";
    if (changed == 2) stale.origin = "https://example.com:8443";
    if (changed == 3) stale.context = "renderer-context-2";
    if (changed == 4) stale.generation++;
    assert(!check.Take(*request, CredentialOperation::Fill, stale, now));
  }
  for (auto operation : {CredentialOperation::Capture, CredentialOperation::Save,
                         CredentialOperation::Update, CredentialOperation::Fill,
                         CredentialOperation::List, CredentialOperation::Delete}) {
    CredentialDocument private_check(true);
    assert(!private_check.Commit(document, true, true, false));
    assert(!private_check.Begin(operation, document, now));
  }
  assert(!check.Commit(document, false, true, false)); // subframe
  assert(!check.Commit(document, true, false, false)); // invalid/unknown TLS
  assert(!check.Commit(document, true, true, true)); // mixed content
  auto insecure = document;
  insecure.origin = "http://example.com";
  assert(!check.Commit(insecure, true, true, false));
  assert(check.Commit(document, true, true, false));
  const auto old = check.Begin(CredentialOperation::Save, document, now);
  check.Invalidate(); // navigation, renderer/context loss, close
  assert(!check.Take(*old, CredentialOperation::Save, document, now));
  auto reload = document;
  reload.generation++;
  reload.context = "reload-context";
  assert(check.Commit(reload, true, true, false));
  assert(!check.Begin(CredentialOperation::Fill, document, now));
  const auto fresh = check.Begin(CredentialOperation::Fill, reload, now);
  assert(fresh && *fresh != *old);
  assert(check.Take(*fresh, CredentialOperation::Fill, reload, now));
  assert(!check.Take(*fresh, CredentialOperation::Fill, reload, now)); // one shot
  const auto expired = check.Begin(CredentialOperation::Fill, reload, now);
  assert(!check.Take(*expired, CredentialOperation::Fill, reload,
                    now + std::chrono::seconds(30)));
  const auto wrong_action = check.Begin(CredentialOperation::List, reload, now);
  assert(!check.Take(*wrong_action, CredentialOperation::Delete, reload, now));
  assert(!check.Take(*wrong_action, CredentialOperation::List, reload, now));
  for (size_t i = 0; i < CredentialDocument::kMaxPending; ++i)
    assert(check.Begin(CredentialOperation::List, reload, now));
  assert(!check.Begin(CredentialOperation::List, reload, now));
  // Expiration restores capacity without reusing request IDs.
  assert(check.Begin(CredentialOperation::List, reload, now + std::chrono::seconds(31)));
}
