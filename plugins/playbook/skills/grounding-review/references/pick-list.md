# Picking items from a list

Use this when a command asks the user to choose some items out of a list: findings to post, or comments to work on. Use the `AskUserQuestion` tool, so the user sees real selection checkboxes. If the tool is not available, fall back to the typed list below.

## With AskUserQuestion

`AskUserQuestion` takes 1 to 4 questions per call, 2 to 4 options per question and a `header` of at most 12 characters. It has no default-checked state, and it adds an "Other" free text entry on its own.

1. **Pick the path.** Ask one single-select question with four options: `Must-fix and questions (recommended)`, `All findings`, `Pick individually` and `None`. For comments to address, use `All comments (recommended)`, `Pick individually` and `None`. The first option is the fast default: it takes the default selection below. A tier name typed into "Other" still works.
2. **Only on `Pick individually`, ask checkbox questions.** Set `multiSelect: true`. For findings, group by label tier: blockers, questions and must-fix, suggestions, then nitpicks and thoughts. Put at most 4 items in a question and at most 4 questions in a call, so 16 items per call. For a longer list, make as many calls as needed, one after the other. Skip a tier with no items.
3. **Write each option** as `N. label file:line`, kept short, with the one-line summary as its description. `N` is the item number in the list printed above the question. Use the tier name in the `header`, such as `Blockers`.
4. **Say what unselected means** in the question text: "Unselected findings are not posted." For comments: "Unselected comments are skipped."
5. **Report-only items.** An item that cannot be picked, such as a finding marked `Report-only`, gets no option. List it in the question text, with its number and why.
6. **Read the answers.** The picked options across all calls are the selection. An "Other" entry is read like a typed reply (see Replies).

In auto mode the `auto-guard` hook denies `AskUserQuestion`: do not ask, apply the default selection and say so in the confirm line.

## Fallback: the typed list

Use this when `AskUserQuestion` is not available: headless `-p`, Codex, or a call the user or a hook denied. Print the list in the chat and read a typed reply.

### The list

Number every item from 1, one line each, with a checkbox. `[x]` means selected by default and `[ ]` means not. Items that cannot be picked, such as a finding marked `Report-only`, keep their number, have no checkbox and say why.

```
Post which findings as a pending review?
  [x] 1. issue (blocking) src/a.rs:10  unchecked index
  [x] 2. question src/b.rs:4  why a copy here?
  [ ] 3. nitpick src/c.rs:9  rename for clarity
  [ ] 4. thought src/d.rs:30  could cache this
Reply with numbers (1,3,5-7), all, none, all except 4, or a tier name. An empty reply takes the marked items.
```

## Default selection

It applies to the fast path, to auto mode and to an empty typed reply.

For findings, select `(blocking)` findings and the labels `issue`, `chore`, `todo`, `typo` and `question`. Leave out `suggestion`, `nitpick`, `thought` and `polish`. For comments to address, select every item.

## Replies

- An empty reply takes the marked items.
- Numbers and ranges: `1,3,5-7`. A range runs low to high.
- `all` takes every pickable item. `none` takes nothing and stops the step.
- `all except 4` or `all except 2,6-7` takes every pickable item but those.
- A tier name still works for findings: `only blockers`, `all must-fix`, `all must-fix + questions`, `all except optional`, `all findings`. Each is a strict superset of the one before. Blocking and questions take precedence, and suggestions and nitpicks stay optional.

A number that is not on the list, a range that runs backward or a word that is not a form above is not guessed at. Name the bad part in one line and ask again, once. A second bad reply takes the marked items and says so.

## Confirm

Before acting, print the choice back in one line: `Selected 1, 2, 5 (3 of 9).` Then go on. The user can still correct it by replying with a new list before the step runs. This holds for both paths.

## Remember the choice

Keep the chosen numbers for the rest of the run. When a later step rebuilds a payload or comes back to the same list, reuse them and do not ask again. A new run asks again, because the findings may differ.

## Flags that skip the question

The command's own flags still skip the question, as the command describes them. A pick list never replaces them.
