# Code intelligence: language servers behind `playbook code`

`playbook code` answers from a local Tree-sitter index. Four commands add exact answers from a language server: `definition`, `hover`, `refs <path>:<line>:<column>` and `diagnostics`. This page records how they work, what they print and the rules that keep them safe.

## What runs when

Each call starts one server, asks one question and stops the server. There is no daemon and no server kept between calls. A call that cannot get an answer from a server answers from the index and says why, and still exits 0.

The steps of a call, in order:

1. Read the target (`<path>:<line>:<column>`, line and column from 1, column counted in characters). A bad target is an argument error and exits 1.
2. Check workspace trust. An untrusted repo starts nothing.
3. Take the workspace lock. A second call at the same time answers from the index with `busy`.
4. Find the server: the `code.lsp.<server>.path` setting, then `PATH`.
5. Start it as the leader of its own process group, send `initialize`, send `didOpen` with the file's text from disk, send one request.
6. Stop the server and kill its whole process group.

## Servers

| Server | Files | Needs |
|---|---|---|
| `typescript-language-server` | `.ts` `.tsx` `.mts` `.cts` `.js` `.jsx` `.mjs` `.cjs` | `node`, and a `typescript` package in the repo |
| `rust-analyzer` | `.rs` | `cargo` and the `rust-src` component |
| `pyright` (`pyright-langserver`) | `.py` `.pyi` | `node`, and a Python environment |
| `gopls` | `.go` | a Go toolchain |

`playbook code lsp list` shows these, and `playbook code lsp status` shows what was found, the version, what is missing and whether this repo is trusted. Neither starts a server, so neither needs trust. Playbook never installs `gopls`: `status` prints the `go install` line.

## The trust gate

Servers run workspace code. `rust-analyzer` runs build scripts and proc macros, Pyright runs the project interpreter, the TypeScript server loads the repo's TypeScript and plugins, and `gopls` runs `go list`. So no server starts until `code.lsp.trustWorkspace` is true for the repo. The setting is read per repo slug, so one setting covers every worktree and branch of that repo. It can be set for one repo or globally, and the org tier refuses it.

```
playbook config set code.lsp.trustWorkspace true
```

Agents must not set it without an explicit instruction from the user. This gate is separate from Claude Code folder trust, which answers a different question.

A trusted `rust-analyzer` writes build output into the repo's `target/` folder.

## Output

Every command takes `--json` and `--timeout <ms>`. Output uses the usual `playbook code` envelope, plus an `lsp` object:

```json
{"lsp": {"state": "ok", "server": "typescript-language-server", "source": "lsp", "reason": null}}
```

`source` is `lsp` for a server answer and `tree_sitter_fallback` otherwise. `state` is one of:

| State | Meaning |
|---|---|
| `ok` | A server answered |
| `untrusted` | The repo is not trusted, so nothing started |
| `busy` | Another call holds the workspace lock |
| `server_missing` | No server for this file, or the configured path is unusable |
| `capability_missing` | The server does not offer this request |
| `timeout` | No answer within the timeout (20 seconds, 60 for `rust-analyzer`) |
| `server_crashed` | The server exited or closed its output |
| `error` | The server returned an error or an answer that did not parse |

Per command:

- `definition` results are `{path, line, column, end_line, end_column}`. The index fallback gives `{path, line, kind, name, signature}` for the name under the cursor.
- `hover` results are `{text}`, cut at 8 KiB, with `truncated` set when cut.
- `refs` at a position gives the same location rows with `confidence: "resolved"`. The fallback gives the name candidates with `confidence: "candidate"`.
- `diagnostics` results are `{path, line, column, severity, message, source, code}`, at most 200, from at most 20 files, with `truncated` set when more exist. With no path it checks the files added or changed since the last index. A server reports as it indexes, so an empty answer may carry the warning "may still be indexing". Without a server the list is empty and `lsp.state` says why.

Locations that are not `file://` paths inside the repo are dropped, counted in `stats.dropped` and named in a warning. In text mode warnings go to stderr.

## Limits

| Limit | Value |
|---|---|
| Message header | 4 KiB |
| Message body (a response) | 8 MiB |
| Request timeout | 20 s, 60 s for `rust-analyzer`, or `--timeout` |
| Shutdown grace | 2 s, then the process group is killed |
| Hover text | 8 KiB |
| Diagnostics | 200 items, 20 files |
| Retries | once, on `ContentModified`, `ServerCancelled`, or an empty answer while the server is still loading |

## Security model

- A server command comes only from the user's own config store, never from a file in the repo.
- Discovery needs an absolute, regular, executable path, and probes `--version` from the system temp dir.
- Every wait is bounded, every read and write runs on its own thread, and the server is killed with its children on return, on error and on SIGINT or SIGTERM (exit 130 or 143).
- Python discovery reads files only. It runs no tool.
- On macOS, if the CLI is killed with SIGKILL, the server and its children exit only when they see their input close. A grandchild that ignores that can outlive the CLI.

## Cold start

Because each call starts its own server, every call pays the server's start. Expect seconds for the TypeScript server and Pyright, and longer for `rust-analyzer` on a large workspace. The timeout and the index fallback bound the cost. A background server pool is the answer if measurement shows start time dominates; it is not built.
