# PR-Agent and the review pipeline

Decision note, 2026-11-05. No code changes with it.

## Question

[PR-Agent](https://github.com/qodo-ai/pr-agent) is an MIT licensed tool that reviews pull requests with a model. Should playbook apply its ideas to the playbook review pipeline (`/playbook:review` and the quick and deep reviews it runs), or to Coda?

## Decision

Apply them to the playbook review pipeline first. Do not adopt PR-Agent itself, and do not start on Coda until the review pipeline has shown which ideas pay off.

## Why

- The review pipeline already exists: `playbook review suggest` routes a change to a quick or a deep review, and the deep review fact-checks its findings before anything is posted. An idea can be tried there with a config key and a test, and removed again.
- PR-Agent runs on its own, in CI or as a service. Playbook reviews run inside the session, with the repo and the memory store at hand. Wrapping PR-Agent would throw those away.
- Coda is outside this repo, so a change there cannot use these review tests and config keys. I could not verify anything about Coda from this repo, and this note does not assess it.

## Ideas worth testing

Each one is a small, separate change to try in the review pipeline. The PR-Agent facts below were checked on 2026-11-05 against its documentation source and its default settings file. The claims about playbook's own commands were checked against this repo.

1. **Score each finding and drop the weak ones.** PR-Agent asks the model to score its own suggestions in a reflection step and drops those under `suggestions_score_threshold`. The deep review could rate each fact-checked finding for impact and certainty, and post only those above a threshold set by a `review.*` key.
2. **Fit a large diff to the context on purpose.** PR-Agent compresses a large PR by ranking files by language and size, and by putting additions ahead of deletions and dropping deletion-only hunks. The deep review could hand each reviewer the files that matter for its angle, with lock files and generated files cut first.
3. **One config file per repo for review tuning.** PR-Agent reads repository overrides from a `.pr_agent.toml` file on the default branch. Playbook already has the `review.*` keys; the open question is whether a repo needs to commit them, which `playbook config` does not do today.

## What would change this

Revisit when a review run shows a clear gap the ideas above do not cover, or when someone wants the same ideas in Coda.

Not verified: how much each idea improves review quality. This note does not measure it, and I did not look for PR-Agent's own figures.
