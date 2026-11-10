---
description: Use at the end of a session, before /clear or compaction, or when asked to wrap up. Writes a handoff for the next session and saves it.
allowed-tools: Bash, Read
argument-hint: "[path]"
model: haiku
effort: low
---

# Session Handoff

Write a handoff for the next session. The facts come from git. You write only the judgment parts. Run every step now and do not ask for confirmation.

1. Read the rules and template. Intent: handoff-template. If the session's tool map (printed at session start) lists it, use that command. Otherwise do this: use the template below. Keep the sections in this order and skip one with nothing real in it. Write for a reader who has not seen this conversation. Decisions and facts only: no tool narration, chronology, praise or speculation. Use short SHAs, path:line, exact versions and ISO dates. No emojis, no em or en dashes, no preamble, no closing offer. Keep each section under about 10 lines.

   ```markdown
   # Session Handoff - <specific one-line topic>

   ## Where we are now
   - <fact>

   ## Decisions made
   - <chosen path>. Why: <reason>.

   ## Next steps
   1. <action>. Why now: <reason>.

   ## Open questions
   - <gap>. Surfaced by: <decision or task>.
   ```

   At a clean stopping point, "Next steps" is the single line `Clean stop. No pending work.` "Open questions" is the only optional section.
2. Gather the facts for "Where we are now". Intent: handoff-facts. If the session's tool map lists it, use that command (point it at `<path>` when step 5 saves for another directory). Otherwise do this: in that directory run `git rev-parse --show-toplevel`, `git branch --show-current`, `git log -1 --format='%h %s'` and `git status --short`, and write bullets for the worktree, the branch (or `(detached)`), HEAD, and the working tree (clean, or the count of changed files with up to 20 listed). Add the last lint, typecheck, test and build results you know of.
3. Write the handoff following the template. Decisions need a reason, next steps need a why-now, and open questions name what surfaced them.
4. Check the shape. Intent: handoff-check. If the session's tool map lists it, use that command, feeding the document through a quoted heredoc (`<<'HANDOFF_EOF'`), never an unquoted heredoc or `echo`, because the facts hold repo text. Otherwise do this: confirm the first three sections are present, in order and not empty, no other `##` heading exists, no `<placeholder>` from the template is left, and no line has an em or en dash. Fix every problem and check again.
5. Save it. Intent: handoff-save. If the session's tool map lists it, use that command with the same quoted heredoc; when a context-size nudge gave you a session id, pass it, and pass `<path>` to save for another directory. Otherwise do this:
   - Folder: `~/.config/playbook/repos/<owner>/<repo>/handoff/`. `<owner>/<repo>` is the output of `git remote get-url origin` with the scheme, any `user@`, the host and a trailing `.git` stripped, so `git@github.com:acme/api.git` and `https://github.com/acme/api.git` both give `acme/api`. With no `origin` remote, use `~/.config/playbook/runtime/handoff/`. Create the folder with `mkdir -p -m 700`.
   - File: `<slug>-<epoch>-<pid>.md`. `<slug>` is the absolute directory path with every character that is not a letter or digit turned into `-` (`printf '%s' "$PWD" | tr -c 'A-Za-z0-9' '-'`), `<epoch>` is `date +%s` and `<pid>` is `$$`. Write it through the same quoted heredoc.
6. If `$ARGUMENTS` holds a path, also write the document there, creating parent directories, and confirm the path in one line. A bare filename goes in the current working directory. Never write to a path the user did not provide.

Print the handoff, then end with one line: tell the user to run `/clear` and `/playbook:session-start` to load it. When the session-start hook is installed, the next session loads it on its own after `/clear`.
