# TODO

Open work for loompa-forge. Move finished items out rather than ticking them,
so the list stays short.

## Now

- [ ] Review Dependabot PR #3 (`actions/checkout` 4 → 7). Major bump; confirm CI
      stays green on `ci.yml` and `release.yml`.
- [ ] Review Dependabot PR #4 (`actions/cache` 4 → 6). Major bump (ESM
      migration); confirm the cargo cache key still restores.

## Next

- [ ] Add a README "Roadmap" line pointing here, so contributors can find it.
- [ ] Decide whether `lf web` needs optional auth for non-loopback binds
      (README currently documents "loopback only, no login").

## Ideas (unscheduled)

- [ ] Cost reporting beyond the daily budget: per-repo or per-schedule totals in
      `lf status`.
- [ ] More built-in agent presets as other CLI agents appear.
