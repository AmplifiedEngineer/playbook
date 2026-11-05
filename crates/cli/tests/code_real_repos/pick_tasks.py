#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
"""Picks 20 benchmark tasks for one real repository from its own symbols.

Usage: pick_tasks.py REPO LANG SEED   (LANG is ts, py or rs)

Reads source files only (never runs or imports them). Ground truth comes from
word matches over the same files, the way `rg -w` finds them:

* declaration: a name declared exactly once in the repo, with its line;
* references: every other line holding the name as a whole word. Four of the
  six tasks use names that occur in at least two files (cross-file imports),
  one stays inside the declaring file, and one is a common identifier (many
  uses, declared once) whose truth is noisy and is reported apart. Lines where
  the name only sits in a comment or a string are listed as `noncode`: no
  language server is expected to return them, so recall is reported with and
  without them;
* symbol_search: a prefix of a declared name, the declaration must come back;
* definition: a cross-file use site of a declared name, must resolve to it;
* file_context: a file and the names declared at its top level.

Prints one JSON document on stdout.
"""
import json
import random
import re
import subprocess
import sys

repo, lang, seed = sys.argv[1], sys.argv[2], int(sys.argv[3])
EXTS = {"ts": (".ts", ".tsx"), "py": (".py",), "rs": (".rs",)}[lang]
DECL = {
    "ts": re.compile(
        r"^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?"
        r"(?:function\*?|class|interface|enum|type|const)\s+([A-Za-z_$][\w$]*)"
    ),
    "py": re.compile(r"^(?:async\s+)?(?:def|class)\s+([A-Za-z_]\w*)"),
    "rs": re.compile(r"^\s*(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait)\s+([A-Za-z_]\w*)"),
}[lang]
WORD = re.compile(r"[A-Za-z_$][\w$]*")
SKIP = ("node_modules/", ".d.ts", "/dist/", "/build/", "/generated/", "vendor/")

files = subprocess.run(
    ["git", "-C", repo, "ls-files"], capture_output=True, text=True, check=True
).stdout.split("\n")
files = [f for f in files if f.endswith(EXTS) and not any(s in f for s in SKIP)]

decls = {}  # name -> [(file, line)]
uses = {}  # name -> {file: set(lines)}
per_file = {}  # file -> [names declared at top level]
for f in files:
    try:
        text = open(f"{repo}/{f}", encoding="utf-8").read()
    except (UnicodeDecodeError, OSError):
        continue
    for n, line in enumerate(text.split("\n"), 1):
        m = DECL.match(line)
        if m:
            decls.setdefault(m.group(1), []).append((f, n))
            per_file.setdefault(f, []).append(m.group(1))
        for w in set(WORD.findall(line)):
            uses.setdefault(w, {}).setdefault(f, set()).add(n)


def is_test(path):
    return bool(re.search(r"(^|/)(tests?|__tests__|testing)/|\.(test|spec)\.|/test_|_test\.", path))


COMMENT = ("#", "//", "*", "/*", '"""', "'''", "<!--")
_lines = {}


def line_text(f, n):
    if f not in _lines:
        _lines[f] = open(f"{repo}/{f}", encoding="utf-8").read().split("\n")
    return _lines[f][n - 1]


def is_noncode(name, f, n):
    """True when every occurrence on the line sits in a comment or a string."""
    text = line_text(f, n)
    if text.strip().startswith(COMMENT):
        return True
    spots = [m.start() for m in re.finditer(r"(?<![\w$])" + re.escape(name) + r"(?![\w$])", text)]
    return bool(spots) and all(
        i > 0 and text[i - 1] in "\"'`" and text[i + len(name) : i + len(name) + 1] in ("\"", "'", "`")
        or "//" in text[:i]
        or "# " in text[:i]
        for i in spots
    )


def refs_of(name):
    (df, dl) = decls[name][0]
    return sorted(
        (f, n) for f, lines in uses[name].items() for n in lines if (f, n) != (df, dl)
    )


unique = {
    n: d[0]
    for n, d in decls.items()
    if len(d) == 1 and len(n) >= 6 and not n.startswith("__") and not is_test(d[0][0])
}
rng = random.Random(seed)


def distinct(names):
    names = sorted(names)
    rng.shuffle(names)
    return names


cross, same, common = [], [], []
for n in unique:
    refs = refs_of(n)
    files_hit = {f for f, _ in refs}
    # Distinctive names only: a lowercase single word is likely a common local.
    shaped = "_" in n or re.search(r"[a-z][A-Z]", n)
    if len(refs) >= 100 and len(files_hit) >= 5:
        common.append(n)
    elif shaped and 3 <= len(refs) <= 60 and len(files_hit - {unique[n][0]}) >= 2:
        cross.append(n)
    elif shaped and 2 <= len(refs) <= 20 and files_hit == {unique[n][0]}:
        same.append(n)

# Spread the cross-file picks over several top-level directories.
chosen_cross, seen_dirs = [], set()
for n in distinct(cross):
    top = "/".join(unique[n][0].split("/")[:2])
    if top in seen_dirs and len(seen_dirs) < 4:
        continue
    seen_dirs.add(top)
    chosen_cross.append(n)
    if len(chosen_cross) == 12:
        break
refs_names = chosen_cross[:4] + distinct(same)[:1]
common_names = sorted(common, key=lambda n: len(refs_of(n)))
common_names = [n for n in common_names if len(refs_of(n)) <= 400][-3:][:1] + common_names[:1]
common_names = list(dict.fromkeys(common_names))[:1]
decl_names = chosen_cross[4:8]
search_names = chosen_cross[8:12]
def_names = chosen_cross[:3]
ctx_files = []
for f, names in per_file.items():
    if 3 <= len(names) <= 25 and not is_test(f):
        ctx_files.append(f)
ctx_files = distinct(ctx_files)[:3]

tasks = []
for n in decl_names:
    f, l = unique[n]
    tasks.append({"kind": "declaration", "query": n, "expected": [[f, l]]})
for n in refs_names + common_names:
    f, l = unique[n]
    tasks.append(
        {"kind": "references", "query": n, "decl": [f, l], "expected": [list(r) for r in refs_of(n)],
         "common": n in common_names,
         "noncode": [list(r) for r in refs_of(n) if is_noncode(n, *r)]}
    )
for n in search_names:
    f, l = unique[n]
    k = max(5, int(len(n) * 0.6))
    tasks.append({"kind": "symbol_search", "query": n[:k], "name": n, "expected": [[f, l]]})
for n in def_names:
    f, l = unique[n]
    site = next(r for r in refs_of(n) if r[0] != f)
    tasks.append({"kind": "definition", "query": n, "site": list(site), "expected": [[f, l]]})
for f in ctx_files:
    tasks.append({"kind": "file_context", "query": f, "names": sorted(set(per_file[f])), "expected": []})

print(json.dumps({"repo": repo, "lang": lang, "seed": seed, "files": len(files), "tasks": tasks}, indent=1))
