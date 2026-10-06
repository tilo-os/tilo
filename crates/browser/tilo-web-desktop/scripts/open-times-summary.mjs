// All requested rounds must satisfy the existing launch-to-first-present budget.
export function summarizeOpenTimes(results, budget) {
  const opens = results.flatMap(result => result.opens);
  const errors = results.flatMap(result => result.errors);
  const stats = rows => {
    const times = rows.map(row => row.ms).filter(Number.isFinite).sort((a, b) => a - b);
    const quantile = q => times[Math.min(times.length - 1, Math.floor(q * times.length))];
    return {opens: rows.length, failed: rows.filter(row => !row.opened || !Number.isFinite(row.ms)).length,
      p50: quantile(0.5), p90: quantile(0.9), max: times.at(-1)};
  };
  const summary = {...stats(opens), first: stats(opens.filter(row => row.round === 1)),
    steady: stats(opens.filter(row => row.round > 1)), budget, errors: errors.slice(0, 10)};
  return {...summary, passed: Number.isFinite(budget) && budget > 0 && summary.opens > 0 && summary.failed === 0
    && summary.max <= budget && errors.length === 0};
}
