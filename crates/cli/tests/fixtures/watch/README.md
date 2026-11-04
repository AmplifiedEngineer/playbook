# Watch mode fixtures

Recorded on 2026-11-04 from the public repository `pragmatic-engineer/playbook` with `gh api -i`. Each endpoint has a `.200.headers` and `.200.json` pair. A `.304.headers` file comes from a second call with `If-None-Match` set to the first ETag. The bodies keep only the fields the poller reads. Logins are replaced by `user-1` and the repository id in `Link` headers by `ID`.

The calls used `since=2026-10-01T00:00:00Z`, `sort=updated`, `direction=asc` and `per_page=2`.

What the recording shows:

- Every endpoint returns an `ETag` and answers `If-None-Match` with `304`.
- Items come back in ascending `updated_at` order from `since`.
- `pull-comments.200.json` is an empty array, because the repository had no review comment in that window. It records the empty page, not the shape of a review comment.
- No endpoint sends `X-Poll-Interval`. The poller treats its absence as the configured `watch.pollSeconds`.
- A `304` carries the rate limit headers and no body.
- `Link: rel="next"` is present when more items exist.
- A push to a pull request branch moves the `updated_at` of that pull request on the issues list: the head commit of one pull request was made at 13:41:40 and the issue `updated_at` read 13:41:42. This is one observation, not a proof for every push.
- Reviews and check runs were not recorded here. They stay probes, as the design says.
