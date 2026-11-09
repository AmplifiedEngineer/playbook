# Code search

<!-- verify: run -->

`playbook code` indexes your repo with tree-sitter, then answers code questions from the index: where a name lives, who uses it and what code a task needs. It is for you and for the Claude Code sessions that work in your repo.

## When to use it

- You want one function, not the whole file around it.
- You work in several worktrees of one repo and do not want to index each one.
- You want a session to spend its tokens on the answer, not on Grep output.

## Try it

The examples on this page run in CI on a small demo repo with two source files, `lib.rs` and `checkout.py`, and one CSV file. Run them in any repo that has an `origin` remote. The index lives outside the repo.

Build the index. It hashes every file git lists and parses only the ones that changed.

```bash
playbook code index
# expect: indexed * files: *
```

```text
indexed 3 files: 3 changed, 0 unchanged, 0 removed, 0 skipped, 0 failed
reused: 0
```

On the playbook repo (1,038 files) the first full build took about 0.8 seconds, measured in [ADR 0056](adr/0056-share-one-code-index-across-worktrees.md). Check that the index matches the files on disk.

```bash
playbook code status
# expect: index: fresh
```

```text
index: fresh
files: 3 ok
```

Ask for the code a task needs, in words. `context` finds matching declarations, widens text hits to the whole declaration around them and fills a token budget, 4,000 tokens by default. Set another with `--max-tokens`, from 200 to 65536.

```bash
playbook code context "how is the cart total computed"
# expect: == checkout.py:4-16  symbol Cart; text; symbol Cart.total_cents
# expect: *def total_cents(self):
```

```text
== checkout.py:4-16  symbol Cart; text; symbol Cart.total_cents
class Cart:
    """Items a customer wants to buy."""

    def __init__(self):
        self.items = []

    def add(self, name, cents):
        """Adds one item to the cart."""
        self.items.append((name, cents))

    def total_cents(self):
        """Returns the cart total in cents."""
        return sum(cents for _, cents in self.items)
```

## The eight commands

| Command | What it does |
|---|---|
| `index` | Builds or refreshes the index. `--rebuild` parses everything again. |
| `status` | Says whether the index matches the files on disk. |
| `search <query>` | Ranks symbol names first, then imports, then text. `--text` searches text only. `--lang`, `--path` and `--limit` narrow it. |
| `symbol <name>` | Finds the declaration of a name. `--kind` picks `function`, `class` or `method`. |
| `outline <path>` | Lists the declarations of one file, indented by nesting. |
| `read <path> <start:end>` | Prints numbered lines, at most 400 lines or 32 KiB. |
| `refs <name>` | Lists the places that spell a name. |
| `context <task>` | Gathers the code a task needs, within a token budget. |

Every command takes `--json`. Search for a name, then narrow it to one language.

```bash
playbook code search shipping
# expect: lib.rs:9-12  function shipping_cents  pub fn shipping_cents(parcel: &Parcel) -> u32
playbook code search total --lang python
# expect: checkout.py:14-16  method Cart.total_cents  def total_cents(self)
```

```text
lib.rs:9-12  function shipping_cents  pub fn shipping_cents(parcel: &Parcel) -> u32
lib.rs:8-8  /// Returns the shipping price in cents for a parcel.
lib.rs:16-16  shipping_cents(parcel) < 100
```

Each hit is a file, a line range, a kind and the line that declares it. Outline a file, then read only the lines you need.

```bash
playbook code outline checkout.py
# expect: class:4-16  Cart  class Cart
# expect: *method:10-12  Cart.add  def add(self, name, cents)
playbook code read lib.rs 9:12
# expect: *9  pub fn shipping_cents(parcel: &Parcel) -> u32 {
```

```text
class:4-16  Cart  class Cart
  method:7-8  Cart.__init__  def __init__(self)
  method:10-12  Cart.add  def add(self, name, cents)
  method:14-16  Cart.total_cents  def total_cents(self)
function:19-21  apply_discount  def apply_discount(total_cents, percent)
```

Find the places that use a name.

```bash
playbook code refs shipping_cents
# expect: lib.rs:9:8  declaration (candidate)  pub fn shipping_cents(parcel: &Parcel) -> u32 {
# expect: lib.rs:16:5  use (candidate)  shipping_cents(parcel) < 100
```

```text
lib.rs:9:8  declaration (candidate)  pub fn shipping_cents(parcel: &Parcel) -> u32 {
lib.rs:16:5  use (candidate)  shipping_cents(parcel) < 100
```

## Languages

Playbook parses these languages:

- Rust
- TypeScript and TSX
- JavaScript
- Python
- Go

It still searches other text files, such as CSV, as plain text, but they get no symbols or outline. `--lang` takes `rust`, `typescript`, `javascript`, `python` or `go`.

## Where the index lives and when it is fresh

One index serves every worktree of a repo, so a new worktree parses only the files that differ. It sits under `~/.config/playbook/data/code/<owner>/<repo>/`, and the index does not write inside your repo. The repo needs an `origin` remote to name that folder.

Search commands run a quick check first. When a file changed after indexing, the command marks its hits `(stale)` and prints a line that tells you to run `playbook code index`. `outline` and `read` use the file on disk every time.

```bash
echo "// edited" >> lib.rs
playbook code status
# expect: index: stale
# expect: *lib.rs
```

```text
index: stale
files: 3 ok
changed (1):
  lib.rs
```

## How sessions use it

Playbook adds a rule at session start: run `playbook code status` first, then `playbook code index` when it says stale or missing, then these commands, all before Grep or Glob. After the first Grep of a session, a hook adds a one-time hint to use the index. Grep and Glob stay the fallback when the binary or the index is missing.

## Limits

`refs` reads syntax only. It skips comments and strings, but it checks no scope or types, so unrelated names with the same spelling also appear. Treat each result as a candidate. `--limit` takes up to 200 results.

## Next

- [Search the code in five commands](recipes/search-the-code.md): the short recipe.
- [Code index evaluation](internals/20-code-index-evaluation.md): how it did against plain Grep.
- [ADR 0056](adr/0056-share-one-code-index-across-worktrees.md): why worktrees share one index.
