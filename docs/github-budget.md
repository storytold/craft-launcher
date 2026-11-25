# GitHub budget

Spec and plan for #713: the launcher asks GitHub's API often enough that users reach GitHub's
rate limit (60 unauthenticated requests an hour per network address, shared by every machine on
it; a 304 to a conditional request still counts unless the request is authenticated). Terms are
in [GLOSSARY.md](../GLOSSARY.md).

## Where the requests came from (main at d2d19f8)

| Path | Before |
|---|---|
| Stable, pre-releases off | Release feed; GitHub only for apps the feed lacks, hourly per app |
| Pre-releases on | Feed skipped; every app asked on every update check (5-minute throttle, 12-minute checks): about 95 an hour |
| Pinned version or Nightly track | Feed and throttle skipped; version list cached 15 minutes in memory, no ETag |
| Developer Builds copy | Every app on the Nightly track, as above, plus nightly discovery paging |
| Forced check | Every throttle skipped |

## Decisions

1. **GitHub budget.** One ceiling per launcher on GitHub API requests, shared by every path: 30
   an hour for the stable launcher, 15 for a Developer Builds copy. Kept on disk as a rolling
   hour in each copy's own data folder; the two copies don't share a file, so the nightly
   storage stays separate.
2. **GitHub's own count.** Stop at the lower of the budget and `X-RateLimit-Remaining` minus 10.
   Each request counts against that number before GitHub answers, a lower count within one
   window stands, and a refill time is trusted for at most an hour. Requests that carry
   `ARTCRAFT_LAUNCHER_GITHUB_TOKEN` have their own limit and skip the budget.
3. **Priority when short.** The user's own action (Choose version, installing a pinned version,
   a forced check, opening Developer Builds), then installed apps, then apps not installed, then
   the Nightly track, then nightly discovery.
4. **Pre-releases on.** The feed still answers between GitHub's asks, which keep the fallback's
   pace (hourly per app once everything has loaded). A newer stable release in the feed replaces
   an older saved beta or RC at once.
5. **Pinned version.** Fetched once and saved; asked again only when the pin changes or the
   saved copy is missing. The version list is kept an hour, on disk; opening Choose version
   asks again for one older than 15 minutes, and a pressed check on the Nightly track asks now.
6. **Nightly track.** At most every 3 hours per app, with its own record apart from the stable
   track's. Nightly discovery for Developer Builds already waits 6 hours between automatic sweeps (`nightly_lookup::EVERY`).
7. **Forced check.** Skips the per-app throttles but not the budget; a second one within 2
   minutes of a forced one reuses its answers (an automatic check doesn't count). A budget
   refusal of nightly discovery isn't saved as a server backoff.
8. **Budget spent.** Saved answers stay. The footer and automation results say when GitHub is
   next asked ("Next GitHub check at 14:05"); a refused forced check is not reported as success.

Out of scope: the web build (it asks from the user's browser with its own logic), a user's own
GitHub token (#1028), and the release feed serving betas, RCs and nightlies (a storyteller-web
change).

## Plan

Three pull requests, each against #713, in this order:

1. **Pre-releases keep to the feed's pace** (decision 4): `check_plan` fetches the feed and keeps
   its GitHub pace with pre-releases on; `latest_for` prefers a newer stable from the feed over a
   saved older pre-release. Adds this document and the glossary terms.
2. **GitHub budget** (decisions 1-3, 7, 8): one budget in `GitHubSource`, charged by every API
   request (`get_api`, `nightly_get`), saved next to the release cache, with priorities passed by
   the caller and the remaining-count header read in `Http`; footer and automation wording.
3. **Pinned and nightly pacing** (decisions 5, 6): pinned releases saved in the release cache,
   the version list cached an hour on disk, and the Nightly track throttled to 3 hours.
