# Recipes

<!-- verify: names -->

Short, copy-pasteable how-tos for the common tasks. CI runs the commands of a `run` recipe in a sandbox: a temp folder, a cleared environment and a small demo repo. It compares each `# expect:` line with the real output. A `names` recipe has every command name checked against the real CLI.

| Task | Recipe | Class |
|---|---|---|
| Start a Claude Code session with Playbook | [start-a-session](start-a-session.md) | names |
| Go from an idea to a built feature | [plan-then-implement](plan-then-implement.md) | names |
| Search your code with the code index | [search-the-code](search-the-code.md) | run |
| Read a PDF or Office file as text | [read-a-document](read-a-document.md) | names |
| Watch your Claude Code costs | [watch-costs](watch-costs.md) | run |
| Check your Playbook setup | [check-your-setup](check-your-setup.md) | names |
| Install Playbook without Homebrew | [install-without-homebrew](install-without-homebrew.md) | names |

`read-a-document` is a `names` recipe because `pbdoc` is a separate binary that the docs checks do not build. `watch-costs` is a `run` recipe for the summary line. Its dashboard and status lines are `names`.
