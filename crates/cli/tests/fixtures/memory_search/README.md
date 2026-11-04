# Memory search fixture

Invented facts and queries for measuring memory recall. No real memory content belongs here.

- facts/: one fact file per id (the file name without .md).
- queries.tsv: first line `version<TAB>1`, then `id<TAB>split<TAB>expected<TAB>query`. `expected` is a comma separated any-of list of fact ids. `split` is `tune` or `score`.
- Tune queries are for choosing weights. Score queries are only for reporting, and need at least 40.
- Files are ASCII with LF endings, marked binary in .gitattributes so git never rewrites them.
- Scan baseline (baseline-scan.json) arrives with the harness that can produce it. A baseline is regenerated only in the same commit as the change that moves hits, and the PR body lists each query whose hits changed.
