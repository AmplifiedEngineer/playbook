# Renamed and removed commands

Deprecated: a renamed command keeps its old name as a stub until 1.0.0. The stub prints one line that names the new command, then runs it, so a saved note or prompt keeps working. The stub is removed in 1.0.0, and that removal is a breaking change. A command removed with no replacement has no stub.

| Old name | New name | Removed in |
| --- | --- | --- |
| `/playbook:commit-and-push` | `/playbook:commit` | 1.0.0 |
| `/playbook:create-pull-request` | `/playbook:pr-create` | 1.0.0 |
