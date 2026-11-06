# Picking items from a numbered list

Use this when a command asks the user to choose some items out of a list: findings to post, or comments to work on. `AskUserQuestion` offers at most four options, so it cannot carry a long list. Print the list in the chat and read a typed reply.

## The list

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

For findings, mark `(blocking)` findings and the labels `issue`, `chore`, `todo`, `typo` and `question`. Leave `suggestion`, `nitpick`, `thought` and `polish` unmarked. For comments to address, mark every item.

## Replies

- An empty reply takes the marked items.
- Numbers and ranges: `1,3,5-7`. A range runs low to high.
- `all` takes every pickable item. `none` takes nothing and stops the step.
- `all except 4` or `all except 2,6-7` takes every pickable item but those.
- A tier name still works for findings: `only blockers`, `all must-fix`, `all must-fix + questions`, `all except optional`, `all findings`. Each is a strict superset of the one before. Blocking and questions take precedence, and suggestions and nitpicks stay optional.

A number that is not on the list, a range that runs backward or a word that is not a form above is not guessed at. Name the bad part in one line and ask again, once. A second bad reply takes the marked items and says so.

## Confirm

Before acting, print the choice back in one line: `Selected 1, 2, 5 (3 of 9).` Then go on. The user can still correct it by replying with a new list before the step runs.

## Remember the choice

Keep the chosen numbers for the rest of the run. When a later step rebuilds a payload or comes back to the same list, reuse them and do not ask again. A new run asks again, because the findings may differ.

## Flags that skip the question

The command's own flags still skip the question, as the command describes them. A pick list never replaces them.
