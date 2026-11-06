# The pbdoc helper

`pbdoc` is the small program that turns one document into Markdown for `playbook doc read`. This page covers how it is built, what it depends on and which limits it enforces. For use, see [Reading documents](../guides/06-reading-documents.md). The design record is [ADR-0051](../adr/0051-bundle-anydoc-as-pbdoc-helper.md).

## Why a separate program

The conversion library has no output cap, no time limit and no memory cap, and a small hostile file can make it use hundreds of megabytes. So conversion runs in a child process that carries every cap, and a blowup stays out of the `playbook` binary. The `playbook` crate does not depend on the converter, and a test checks that.

## Command line

```
pbdoc [--max-input-bytes N] [--max-memory-mb N] -o <out.md> -- <input>
pbdoc --version            prints "pbdoc <playbook version>"
pbdoc --converter-version  prints "anydoc 0.2.4"
```

Any other flag is a usage error. No flag turns on OCR, a URL, an API key or a hosted mode. On failure it prints one `error: <message>` line on stderr, never any document text.

| Exit code | Meaning |
| :- | :- |
| 0 | The Markdown file was written. |
| 1 | The conversion failed, or the input could not be read. |
| 2 | Usage error. |
| 3 | The document has no text layer and needs OCR. |
| 4 | A safety limit was hit. |

`playbook doc read` has its own codes: 0 on success, 1 for a failure, 2 for a section number that does not exist, 3 when the document needs OCR, and 128 plus the signal number when interrupted.

## Dependencies

- `anydoc`, pinned to 0.2.4 with default features off. It does the conversion.
- `rayon`, pinned, to set the worker pool to one thread. A PDF would otherwise spread across many threads and add up their memory.
- `zip`, pinned, with only deflate. `pbdoc` uses it to read the central directory of a zip based file before conversion.

A test reads `cargo metadata` and fails when the helper's closure holds a network crate (such as hyper, reqwest or tokio), a crate with `ocr` or `tesseract` in its name or a licence outside the permissive list. Another test fails when a closure crate is missing from `NOTICE-pbdoc`. Because of these, a dependency bump that adds a crate cannot ship unnoticed.

## What one run does

1. Start a watchdog thread and set the worker pool to one thread.
2. Check the input from its metadata alone. It must be a regular file no larger than the lower of `--max-input-bytes` and 100 MiB.
3. For a zip based file, read only the central directory and refuse it when it declares too much.
4. Read the whole file into memory and convert from those bytes.
5. Check the memory use again and write the Markdown to the output path.

`pbdoc` never unpacks an archive onto the disk. Its only write is the one output file.

## Limits inside the helper

| Limit | Value |
| :- | :- |
| Input size | 104,857,600 bytes (100 MiB), whatever `--max-input-bytes` says |
| Zip entries | at most 10,000 |
| Zip uncompressed size, summed | at most 268,435,456 bytes (256 MiB) |
| Zip compression ratio | at most 100 for an entry of 1 MiB or more |
| Memory | `--max-memory-mb`, default 1024, never above 4096 |

A zip that breaks a limit exits 4. The memory limit is a resident size check. A watchdog thread reads the helper's own size every 20 ms on Linux and every 100 ms on macOS, where each reading is a `ps` call, and the helper also checks after reading the input and after converting. A spike shorter than the interval can pass between readings.

## Limits added by playbook

`playbook doc read` starts the helper with these on top:

- A scrubbed environment and a private scratch folder.
- No network: the macOS sandbox or a Linux network namespace when one works.
- On Linux only, an address space cap of the memory limit plus 512 MiB. macOS refuses that kind of cap, so there the watchdogs are the memory limit.
- A limit of 64 MiB on any file the child writes, and a read of at most 32 MiB of its output.
- Polling of the process group size every 100 ms, and a kill of the whole group over the cap or the timeout.

A helper that dies on a crash signal is reported as a safety limit hit, because a refused allocation in Rust aborts the process.

## Where it is found and shipped

The lookup order is `docs.anydocPath`, then the `pbdoc` next to the `playbook` binary, then `anydoc` on `PATH`. The release builds `pbdoc` for macOS and Linux and publishes it as its own archive with `NOTICE-pbdoc`. The build fails when the binary passes 8 MiB (8,388,608 bytes) or the archive passes 4.5 MiB (4,718,592 bytes). `playbook update` replaces the helper, `playbook uninstall` removes it and `playbook doc status` shows which converter is in use. There is no helper on Windows.
