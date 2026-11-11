# Telemetry

`playbook setup` asks whether to send Claude Code telemetry to a collector on your own machine. The answer defaults to yes. It writes eight OpenTelemetry variables into the `env` block of `~/.claude/settings.json`, so plain `claude`, the IDE and the desktop app feed `playbook usage`.

Prompt text and tool content are never sent. Nothing leaves your machine unless you turn on [usage sync](17-sync.md), which is off by default. The collector listens on 127.0.0.1 only. Playbook's `session-init` hook (part of the hooks you install with setup) starts it when a session begins. Without those hooks, start it with `playbook usage collect`, or let `playbook cc` do it.

- To turn it on later, or after you declined: `playbook setup --only telemetry`.
- `playbook config set usage.telemetry auto` writes the same keys, and `... off` removes only those keys and puts back any value setup replaced. If telemetry is set up elsewhere, `auto` leaves it as it is and says so. Both print each key they touch with a one-line reason, the file path, the saved copy of the old file and the undo command. A settings file that is not valid JSON is left untouched.
- If `CLAUDE_CODE_ENABLE_TELEMETRY` is 0 in `~/.claude/settings.json` and nothing else sets telemetry, `--yes` leaves it alone. Answer yes to the question or run `playbook setup --only telemetry`.
- Claude Code sends no telemetry from interactive sessions while `IS_DEMO` is set to anything but `0`. When the `telemetry` part runs and `~/.claude/settings.json` holds such an `IS_DEMO`, setup removes it, prints one line that says so, and records the old value so `playbook uninstall` puts it back. It does not add `IS_DEMO`. If `IS_DEMO` comes from another settings file or your shell, setup names that source and leaves it, and each interactive session says why it sends nothing. To stop that message, remove `IS_DEMO` there, or turn telemetry off with `playbook config set usage.telemetry off`.
- If some of the keys are missing or point at an old port or token, setup puts them right. It does so when a record of what setup wrote exists, when only `CLAUDE_CODE_ENABLE_TELEMETRY` is set, or when all eight keys are set with a local endpoint and a bearer token. A setup that names another collector is left alone.
- If telemetry is set in project settings, your shell or next to other `OTEL_` keys, setup leaves all of it as it is (see [What setup installs](05-what-setup-installs.md)).
- `DISABLE_TELEMETRY` and `DO_NOT_TRACK` only turn off Anthropic's own analytics and are left as they are.
- Restart Claude Code after setup, because it reads settings at start.
- `playbook doctor check` checks that settings point at the collector, that the token Claude Code sends is the collector's, that it runs, and that rows arrived in the last day.
- When usage sync is on, `playbook doctor check` also shows one `sync` row with the age of the last success and the rows pending. With sync off there is no row.
- `playbook uninstall` removes exactly the keys setup wrote and puts back any value they replaced, unless you changed a key since.

See also [Usage view](../internals/04-usage-dashboard.md), [pbtop](11-pbtop.md) and [usage sync](17-sync.md).
