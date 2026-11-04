# PR-Agent and the review pipeline

Decision note, 2026-11-04. No code changes with it.

## Question

[PR-Agent](https://github.com/qodo-ai/pr-agent) is an open source tool that reviews pull requests with a model. Should playbook apply its ideas to the playbook review pipeline (`/playbook:review`, `quick-review`, `deep-review`), or to Coda?

## Decision

Apply them to the playbook review pipeline first. Do not adopt PR-Agent itself, and do not start on Coda until the review pipeline has shown which ideas pay off.

## Why

- The review pipeline already exists and has its own measurements: `playbook review suggest` routes a change to a quick or a deep review, and the deep review fact-checks its findings before anything is posted. An idea can be tried there with a config key and a test, and removed again.
- PR-Agent runs outside the session, as a bot or an action. Playbook reviews run inside the session, with the repo, the memory store and the fact-checker at hand. Wrapping PR-Agent would throw those away.
- Coda is outside this repo, so a change there cannot use these review tests and config keys, and this note does not assess it.

## Ideas worth testing

These come from PR-Agent's public documentation and were not re-verified against its source for this note. Each one is a small, separate change to try in the review pipeline.

1. **Score each finding and drop the weak ones.** PR-Agent asks the model to rate its own suggestions and filters on the rating. The deep review could rate each fact-checked finding for impact and certainty, and post only those above a threshold set by a `review.*` key.
2. **Fit a large diff to the context on purpose.** PR-Agent compresses a large PR by ranking files and trimming low value hunks. The deep review could hand each reviewer the files that matter for its angle, with lock files, generated files and renames cut first.
3. **One config file per repo for review tuning.** PR-Agent reads its settings from a file in the repo. Playbook already has the `review.*` keys; the open question is whether a repo needs to commit them, which `playbook config` does not do today.

## What would change this

Revisit when a review run shows a clear gap the ideas above do not cover, or when someone wants the same ideas in Coda.
