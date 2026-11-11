# Confluence with acli

Read this when you need to find or read Confluence pages.

**Source of truth is the binary.** The published acli reference (https://developer.atlassian.com/cloud/acli/reference/commands/) covers Jira, auth and admin but not the Confluence subcommands. Everything below was read from `--help` on acli 1.3.39-stable (`acli confluence --help` lists `auth`, `blog`, `page`, `space`). After an acli upgrade, re-check with `acli confluence <group> <command> --help`.

## Commands

| Group | Commands |
|---|---|
| `auth` | `login`, `logout`, `status`, `switch` |
| `page` | **`view` only** |
| `blog` | `create`, `list`, `view` |
| `space` | `archive`, `create`, `list`, `restore`, `update`, `view` |

## Discovering pages: space to homepage to children

```bash
# 1. Find the space and its homepage page ID
acli confluence space list --json --expand homepage --limit 100

# 2. Walk down from the homepage, one level at a time
acli confluence page view --id <homepage-id> --json --include-direct-children

# 3. Recurse into whichever children look relevant
acli confluence page view --id <child-id> --json --include-direct-children
```

`--include-labels` on `page view` is the cheapest way to spot runbooks, decision records and onboarding pages without reading every body.

`page view` flags: `--id`, `--json`, `--body-format storage|atlas_doc_format|view`, `--get-draft`, `--status current,draft,archived`, `--version <n>`, and the `--include-*` flags: `collaborators`, `direct-children`, `favorited-by-current-user-status`, `labels`, `likes`, `operations`, `properties`, `version`, `versions`, `webresources`.

`blog list` filters: `--space-id`, `--title`, `--id`, `--status`, `--sort`, `--limit` (default 25), `--cursor`, `--body-format`, and `--json` or `--csv`. `space view` takes `--id` plus `--labels`, `--permissions`, `--properties`, `--operations`, `--include-all`.

`auth` has `login` (OAuth or API token), `logout`, `status` and `switch`. Confluence auth is separate from Jira auth.

## Write commands (verified `--help`, 1.3.39-stable)

These change Confluence state. Never run them during research.

- `space create`: `--key`, `--name`, `--description`, `--alias`, `--private`, `--template-key`, `--json`.
- `space update`: `--key`, `--name`, `--description`, `--status`, `--type`, `--json`.
- `space archive` and `space restore`: `--key`, `--json`.
- `blog create`: `--space-id`, `--title`, `--body` (storage format XHTML), `--from-file`, `--from-json`, `--generate-json`, `--status` (for example `draft`), `--private`, `--created-at`, `--json`.

Read differences to watch: `blog view` takes `--id`, `--body-format` (default `view`), `--draft`, `--version`, `--include labels,properties,...` and `--json`, while `page view` uses `--get-draft` and separate `--include-*` flags. `auth login` takes `--web` for OAuth, or `--site`, `--email` and `--token` (token read from standard input) for an API token.

## What acli can and cannot do for Confluence

Can: view a page by ID, view and list spaces, list and view blog posts, create blogs, create and update spaces. Cannot: search pages, list pages, create or edit pages. For those use the MCP server or twg (`twg confluence search text`, `twg confluence content list`), see references/twg.md in this skill.

`space list` filters: `--keys`, `--type global|personal`, `--status current|archived`, `--limit` (default 50), `--expand description,homepage,permissions`.

A page ID from a browser URL sits in the path (`/pages/123456789/Title`); that number is what `--id` wants.

## Using it in /playbook:learn

The confluence collector wants pages on setup, onboarding, architecture, runbooks and decisions. With no page search, the sequence is:

1. `acli confluence auth status`. If it fails, mark Confluence unavailable and record the reason rather than retrying.
2. `acli confluence space list --json --expand homepage` and pick the space from the README or repo links.
3. Walk the homepage's direct children, then recurse only into branches whose titles or labels match.
4. Fetch bodies with `--body-format storage` only for pages you keep. It is the expensive part.

Cap the walk. A large space is thousands of pages and there is no server-side filter.
