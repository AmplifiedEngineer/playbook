# Memory search fixture

Invented facts and queries for measuring memory recall. No real memory content belongs here.

- facts/: one fact file per id (the file name without .md).
- queries.tsv: first line `version<TAB>1`, then `id<TAB>split<TAB>expected<TAB>query`. `expected` is a comma separated any-of list of fact ids. `split` is `tune` or `score`.
- Tune queries are for choosing weights. Score queries are only for reporting, and need at least 40.
- Files are ASCII with LF endings, marked binary in .gitattributes so git never rewrites them.
- baseline-scan.json: the ordered top 3 the shipping scan returns for every query over the fixture plus generated distractors (2000 facts). The corpus is built from a pinned generator and its hash is recorded in the file. A baseline is regenerated only in the same commit as the change that moves hits, with `UPDATE_MEMORY_SEARCH_BASELINE=1 cargo test -p playbook --test memory_search_quality`, and the PR body lists each query whose hits changed.
- The ignored tests in memory_search_quality.rs measure a 50k fact corpus, the session start index build at 10k and 50k facts, and the developer's own store. Run them by hand with `--ignored`, in release mode.
