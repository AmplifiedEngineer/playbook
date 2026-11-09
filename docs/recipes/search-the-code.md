# How to search your code with the code index

<!-- verify: run -->

Index the repo once, then find symbols and read just the lines you need.

**You need:** the `playbook` binary and a git repo with an `origin` remote. Nothing leaves your machine.

1. Build the index. It reads only the files that changed.

   ```bash
   playbook code index
   # expect: indexed * files: *
   ```

2. Check that it is fresh.

   ```bash
   playbook code status
   # expect: index: fresh
   ```

3. Search by name or by words.

   ```bash
   playbook code search shipping
   # expect: lib.rs:9-12  function shipping_cents  pub fn shipping_cents(parcel: &Parcel) -> u32
   ```

4. Jump to one symbol, then read it with its context.

   ```bash
   playbook code symbol shipping_cents
   playbook code context shipping_cents
   # expect: == lib.rs:9-12  symbol shipping_cents; text
   ```

## What you see

Each result is a file, a line range, a kind and the signature. `context` prints whole declarations, so a session does not read a whole file for one function.

## Next

- [Code search](../code-search.md): all eight commands and their limits.
- [Code index evaluation](../internals/20-code-index-evaluation.md): how the index did against plain Grep.
