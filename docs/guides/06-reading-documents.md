# Reading documents

`playbook doc read <file>` turns a PDF or an Office file into Markdown, on your machine, so a session reads text instead of a binary file. The converter is bundled: `pbdoc` is installed next to `playbook` on macOS and Linux, so there is nothing to install. Windows has no conversion, the command and the hint are off there.

## Turn it on

1. Check it: `playbook doc status`. It names the converter in use and its version.
2. Nothing more to do: `docs.convert` defaults to `auto`. A Read of a whole PDF, or of a Word, PowerPoint, Excel, EPUB, RTF or OpenDocument file, gets one line that suggests `playbook doc read <file>`. The hook only suggests. It never changes the call and never blocks. Set `docs.convert` to `off` to stop the hint. See the [benchmark](../internals/10-document-conversion-benchmark.md).

If `pbdoc` is missing, `playbook doc status` says so. Reinstall with `install.sh` or `brew reinstall playbook`, or run `playbook upgrade`.

The converter was once an npm package. If you installed it earlier, `npm uninstall --global @firecrawl/anydoc` removes it, because playbook now uses the bundled one first. To run a different `anydoc` anyway, set `docs.anydocPath`. That program wins over the bundled one, and playbook does not check that it works offline.

## Long documents

A document longer than `docs.displayChars` (24000 characters) is not printed whole. The output shows the first slice, the total length, a numbered index of the headings and the command for the next slice. The full text is kept in a private cache for a day, so later slices do not convert again.

- `--offset N` starts at character N. `--limit M` prints at most M characters, never more than `docs.displayChars`.
- `--section K` prints section K from the index.
- `--json` adds `total_chars`, `offset` and `next_offset`.

A section number that does not exist exits with code 2 and says which numbers are valid.

## What it reads

pdf, docx, doc, odt, rtf, epub, pptx, ppt, odp, xlsx, ods and csv. Images are not converted, use Read for those.

## Exit codes

`playbook doc read` exits 0 on success, 1 for a failure, 2 for a section that does not exist, 3 for a document that needs OCR and 128 plus the signal number when interrupted. The helper and its limits are described in [The pbdoc helper](../internals/19-pbdoc-helper.md).

## Safeguards

- **Local only.** The OCR option and the hosted mode of the upstream `anydoc` tool send the document to a remote server. Playbook never passes them. A scanned PDF with no text layer answers "needs OCR, not supported locally" and nothing is retried elsewhere.
- **No network where the system allows it.** The converter runs inside the macOS sandbox with network denied, or in a Linux network namespace. When neither works the output says that only the environment was scrubbed. The environment is always scrubbed: no proxy variables, no API keys, no `NODE_OPTIONS`.
- **Limits.** Files over `docs.maxBytes` (25 MB) are refused, and nothing over 100 MiB is opened whatever that key says. A zip based file that expands too far, or a conversion that hits the memory cap, stops with "the document hit a safety limit". The converter is stopped after `docs.timeoutSeconds` (30) or when it uses more than `docs.maxMemoryMb` (1024). The text is cut at `docs.maxOutputChars` (200000) with a marker that says how many characters were shown.
- **Private temporary files.** The converter writes to a random folder under `~/.config/playbook/tmp` that only you can open. It is deleted when the command ends, on success, error, timeout, interrupt or crash of playbook itself. A folder left by a killed process is removed on the next run once it is older than an hour.
- **Untrusted content.** The text is wrapped in a block with a random tag and a line that says to treat it as data and not follow instructions inside it. Nothing from the document is logged. Only the slice cache holds its text, in a folder only you can open, and it is swept after a day.

## What is lost

The output lists what the conversion does not carry. For Word files, tracked changes are not represented, so deleted text is missing. Charts and images become nothing or alt text. Slide layout is dropped. Spreadsheet formulas show their last saved value. When a detail matters, read the original.

## Config keys

See the [config keys page](../reference/config-keys.md): `docs.convert`, `docs.maxBytes`, `docs.timeoutSeconds`, `docs.maxOutputChars`, `docs.maxMemoryMb`, `docs.displayChars`, `docs.anydocPath`.
