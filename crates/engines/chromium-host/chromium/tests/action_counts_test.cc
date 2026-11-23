#include "action_counts.h"
#include <cassert>
#include <string>

namespace {
tilo::RuleAction At(double time, std::string owner = {}) {
  return tilo::RuleAction{"https://ads.example/a.js", "script", "get", "https://news.example", std::move(owner), time};
}
}  // namespace

int main() {
  const std::string blocker(32, 'a'), other(32, 'b');
  tilo::ActionCounts counts;

  // A page starts loading, commits, and its blocked requests count for the
  // extension whose rules blocked them, each once.
  counts.Loading(1, 1000);
  counts.Navigated(1, 1200);
  auto first = counts.Add(1, At(1300));
  auto second = counts.Add(1, At(1400));
  assert(first && second && *first != *second);
  assert(counts.Count(1, *first, blocker));
  assert(!counts.Count(1, *first, blocker));
  assert(counts.Count(1, *second, blocker));
  assert(counts.Count(1, *second, other));
  assert(counts.Of(1, blocker) == 2 && counts.Of(1, other) == 1);
  assert(counts.Of(2, blocker) == 0);
  assert(counts.Actions(1).size() == 2);

  // An extension that starts showing its count counts the page so far by
  // the keys kept with it, each once.
  const std::string newcomer(32, 'c');
  auto keyed = counts.Keyed(1);
  assert(keyed.size() == 2 && keyed[0].first == *first && keyed[1].first == *second && keyed[1].second.time == 1400);
  for (const auto& entry : keyed) assert(counts.Count(1, entry.first, newcomer));
  assert(counts.Of(1, newcomer) == 2 && !counts.Count(1, *first, newcomer));
  assert(counts.Keyed(2).empty());
  counts.Forget(newcomer);

  // The extension adjusts its own count, never below zero.
  counts.Increment(1, blocker, 3);
  assert(counts.Of(1, blocker) == 5);
  counts.Increment(1, blocker, -9);
  assert(counts.Of(1, blocker) == 0);

  // The next page: a request of its own acted on before the host heard it
  // committed is its own; one acted on before it began loading is not, nor
  // is one reported only now from the old page.
  auto early = counts.Add(1, At(2050));
  assert(early && counts.Count(1, *early, blocker));
  counts.Loading(1, 2000);
  counts.Navigated(1, 2100);
  assert(counts.Of(1, blocker) == 1 && counts.Of(1, other) == 0);
  assert(counts.Actions(1).size() == 1);
  assert(!counts.Add(1, At(1500)));
  assert(!counts.Count(1, *first, blocker));

  // A page that took long to commit keeps only the last second before it.
  counts.Loading(1, 3000);
  auto late = counts.Add(1, At(8500));
  auto stale = counts.Add(1, At(3500));
  assert(late && stale);
  counts.Navigated(1, 9000);
  assert(counts.Actions(1).size() == 1 && counts.Actions(1)[0].time == 8500);

  // A redirect names the extension whose file it went to.
  auto redirected = counts.Add(1, At(9100, blocker));
  assert(redirected && counts.Actions(1).back().owner == blocker);

  // A long page keeps its newest requests; the count goes on.
  for (int i = 0; i < 1100; ++i) {
    auto key = counts.Add(3, At(10000 + i));
    assert(key && counts.Count(3, *key, other));
  }
  assert(counts.Actions(3).size() == tilo::ActionCounts::kMaxActions);
  assert(counts.Actions(3).front().time == 10100);
  assert(counts.Of(3, other) == 1100);

  // An extension removed counts no more; a closed tab keeps nothing.
  counts.Forget(other);
  assert(counts.Of(3, other) == 0);
  counts.Removed(3);
  assert(counts.Actions(3).empty());
}
