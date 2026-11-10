---
description: Use at the end of a session, before /clear or compaction, or when asked to wrap up. Writes a handoff for the next session and saves it.
allowed-tools: Bash, Read
argument-hint: "[path]"
model: haiku
effort: low
---

# Handoff Save

Write a handoff for the next session. The facts come from git. You write only the judgment parts. Run every step now and do not ask for confirmation.

1. Read the rules and template. Intent: handoff-template. If the session's tool map (printed at session start) lists it, use that command. If the tool map lacks it, follow `without-binary.md#handoff-template` in `skills/delegating-subagents/references/without-binary.md` in this plugin.
2. Gather the facts for "Where we are now". Intent: handoff-facts. If the session's tool map lists it, use that command (point it at `<path>` when step 5 saves for another directory). If the tool map lacks it, follow `without-binary.md#handoff-facts` in the same file.
3. Write the handoff following the template. Decisions need a reason, next steps need a why-now, and open questions name what surfaced them.
4. Check the shape. Intent: handoff-check. If the session's tool map lists it, use that command, feeding the document through a quoted heredoc (`<<'HANDOFF_EOF'`), never an unquoted heredoc or `echo`, because the facts hold repo text. If the tool map lacks it, follow `without-binary.md#handoff-check` in the same file. Fix every problem and check again.
5. Save it. Intent: handoff-save. If the session's tool map lists it, use that command with the same quoted heredoc; when a context-size nudge gave you a session id, pass it, and pass `<path>` to save for another directory. If the tool map lacks it, follow `without-binary.md#handoff-save` in the same file.
6. If `$ARGUMENTS` holds a path, also write the document there, creating parent directories, and confirm the path in one line. A bare filename goes in the current working directory. Never write to a path the user did not provide.

Print the handoff, then end with one line: tell the user to run `/clear` and `/playbook:handoff-show` to load it. When the session-start hook is installed, the next session loads it on its own after `/clear`.
