// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Port of the retired shell original, validating every `agents/*.md`
//! definition (excluding the `_TEMPLATE.md` skeleton) against the house
//! agent contract: real frontmatter, the required keys, a `name` matching
//! the filename, an allowed model tier and effort level, a known tool name
//! allowlist, a read-only tool allowlist enforced by tier, and the
//! non-negotiable guardrail invariants (heading, no dashes, grounding, zero
//! AI attribution), all matched inside the guardrails section.
//!
//! Deliberately NOT reusing `hooks::rebuild_memory_graph`'s hand-rolled
//! YAML-subset parser: that one builds typed scalar/list/dict values for
//! arbitrary nested frontmatter, while this module only ever needs the
//! trimmed, once-double-quote-unwrapped text after the first colon on an
//! unindented "key: value" line, exactly what `check-agents.sh`'s own
//! `frontmatter_value` does with `grep`/`sed`. Bending that heavier parser's
//! semantics to fit a script this narrow would cost more than it saves.

use std::fs;
use std::path::{Path, PathBuf};

const ALLOWED_MODELS: [&str; 3] = ["haiku", "sonnet", "opus"];
const ALLOWED_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Two read-only tiers, matched strictest first. "Structurally read-only"
/// agents (agents/reviewer.md) hold no Bash at all. Plain "read-only" agents
/// (agents/auditor.md) may hold Bash for non-mutating shell like git log.
const FORBIDDEN_TOOLS_STRICT: [&str; 4] = ["Edit", "Write", "NotebookEdit", "Bash"];
const FORBIDDEN_TOOLS_LOOSE: [&str; 3] = ["Edit", "Write", "NotebookEdit"];
/// Every tools entry, in every agent regardless of tier, must be one of
/// these. Derived from the tools agents/*.md and commands/*.md frontmatter
/// actually use, plus the forbidden set above.
const ALLOWED_TOOLS: [&str; 13] = [
    "Agent",
    "AskUserQuestion",
    "Bash",
    "Edit",
    "Glob",
    "Grep",
    "NotebookEdit",
    "Read",
    "Skill",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Write",
];
const GUARDRAILS_HEADING: &str = "## Non-negotiable guardrails";
const TEMPLATE_NAME: &str = "_TEMPLATE.md";

/// Why the frontmatter delimiters were unparsable, mirroring the exit codes
/// `check-agents.sh:frontmatter_body` returns (1 and 2).
enum DelimiterError {
    MissingOpening,
    MissingClosing,
}

/// The trimmed, first-match value of a top-level "<key>: value" line in
/// `body`, or `None` if the key is absent. Only a value wrapped in DOUBLE
/// quotes is unwrapped, matching the shell's literal `'"'` comparisons: a
/// single-quoted value passes through with its quotes intact.
fn frontmatter_value(body: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let line = body.split('\n').find(|l| l.starts_with(&prefix))?;
    let value = line[prefix.len()..].trim();
    let bytes = value.as_bytes();
    let is_quoted = value.len() >= 2 && bytes[0] == b'"' && bytes[value.len() - 1] == b'"';
    Some(if is_quoted {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    })
}

/// The frontmatter body strictly between the opening and closing `---`
/// lines. Both delimiter lines must be exactly "---" (no trailing
/// whitespace tolerated), mirroring the shell's `[[ "$first_line" == "---"
/// ]]` and awk's `$0=="---"` rather than a looser YAML-parser notion of a
/// delimiter line.
fn frontmatter_body(content: &str) -> Result<String, DelimiterError> {
    let mut lines = content.split('\n');
    if lines.next() != Some("---") {
        return Err(DelimiterError::MissingOpening);
    }
    let mut body_lines = Vec::new();
    for line in lines {
        if line == "---" {
            return Ok(body_lines.join("\n"));
        }
        body_lines.push(line);
    }
    Err(DelimiterError::MissingClosing)
}

/// The five required frontmatter keys, the name-to-filename match, and the
/// model and effort enums.
fn check_required_keys(file: &str, name: &str, body: &str) -> Vec<String> {
    let mut violations = Vec::new();

    match frontmatter_value(body, "name") {
        Some(value) if value == name => {}
        Some(value) => violations.push(format!(
            "{file}: name '{value}' does not match filename '{name}'"
        )),
        None => violations.push(format!("{file}: missing required frontmatter key 'name'")),
    }

    if frontmatter_value(body, "description").is_none() {
        violations.push(format!(
            "{file}: missing required frontmatter key 'description'"
        ));
    }

    if frontmatter_value(body, "tools").is_none() {
        violations.push(format!("{file}: missing required frontmatter key 'tools'"));
    }

    match frontmatter_value(body, "model") {
        Some(value) if ALLOWED_MODELS.contains(&value.as_str()) => {}
        Some(value) => violations.push(format!(
            "{file}: model '{value}' is not one of: {}",
            ALLOWED_MODELS.join(" ")
        )),
        None => violations.push(format!("{file}: missing required frontmatter key 'model'")),
    }

    match frontmatter_value(body, "effort") {
        Some(value) if ALLOWED_EFFORTS.contains(&value.as_str()) => {}
        Some(value) => violations.push(format!(
            "{file}: effort '{value}' is not one of: {}",
            ALLOWED_EFFORTS.join(" ")
        )),
        None => violations.push(format!("{file}: missing required frontmatter key 'effort'")),
    }

    violations
}

/// The tool name allowlist, which every agent's tools list must satisfy
/// regardless of tier, plus the two read-only tiers read off the
/// description wording.
fn check_tools(file: &str, body: &str) -> Vec<String> {
    let mut violations = Vec::new();
    // Neither call distinguishes "key absent" from "key present but empty":
    // the shell assigns whatever frontmatter_value printed either way and
    // never checks its return code here.
    let description = frontmatter_value(body, "description").unwrap_or_default();
    let tools_value = frontmatter_value(body, "tools").unwrap_or_default();
    if tools_value.is_empty() {
        return violations;
    }

    let tokens: Vec<String> = tools_value
        .split(',')
        .map(|t| t.trim().to_string())
        .collect();

    let unknown: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|t| !ALLOWED_TOOLS.contains(t))
        .collect();
    if !unknown.is_empty() {
        violations.push(format!(
            "{file}: tools lists unknown tool name(s) not in the allowlist: {}",
            unknown.join(", ")
        ));
    }

    if description.is_empty() {
        return violations;
    }

    let lower_description = description.to_lowercase();
    let (forbidden, tier_reason): (&[&str], &str) =
        if lower_description.contains("structurally read-only") {
            (
                &FORBIDDEN_TOOLS_STRICT,
                "structurally read-only, tools must not include Edit, Write, NotebookEdit, or Bash",
            )
        } else if lower_description.contains("read-only") {
            (
                &FORBIDDEN_TOOLS_LOOSE,
                "read-only, tools must not include Edit, Write, or NotebookEdit",
            )
        } else {
            return violations;
        };

    let offending: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|t| forbidden.contains(t))
        .collect();
    if !offending.is_empty() {
        violations.push(format!(
            "{file}: description declares the agent {tier_reason}, found: {}",
            offending.join(", ")
        ));
    }

    violations
}

/// `^[a-zA-Z_-]+:` matched against a whole line: returns the key text
/// (before the first colon) when the line's leading run of letters,
/// underscores, and hyphens is immediately followed by a colon. No leading
/// whitespace is tolerated, so indented sub-keys never match.
fn top_level_key(line: &str) -> Option<&str> {
    let end = line.find(|c: char| !(c.is_ascii_alphabetic() || c == '_' || c == '-'))?;
    if end == 0 || line.as_bytes().get(end) != Some(&b':') {
        return None;
    }
    Some(&line[..end])
}

/// A value already wrapped in a matching pair of double or single quotes:
/// the colon-space inside it is inert to YAML.
fn is_quoted_scalar(value: &str) -> bool {
    let bytes = value.as_bytes();
    let len = bytes.len();
    len >= 2
        && ((bytes[0] == b'"' && bytes[len - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[len - 1] == b'\''))
}

/// Reject a frontmatter value real YAML would refuse: an unquoted scalar
/// containing a colon-space, which YAML reads as a nested mapping. Every
/// other rule reads values with `frontmatter_value`, which is far more
/// forgiving than a YAML parser, so this is the one rule that catches a
/// description like "the orchestrator's prompt: a single named lens" that
/// would otherwise silently drop every frontmatter field at runtime.
fn check_yaml_scalars(file: &str, body: &str) -> Vec<String> {
    let mut violations = Vec::new();
    for line in body.split('\n') {
        let Some(key) = top_level_key(line) else {
            continue;
        };
        let value = line[key.len() + 1..].trim();
        if value.is_empty() || is_quoted_scalar(value) {
            continue;
        }
        if value.contains(": ") {
            violations.push(format!(
                "{file}: frontmatter '{key}' is an unquoted value containing a colon-space, \
                 which YAML parses as a nested mapping; wrap the value in double quotes"
            ));
        }
    }
    violations
}

/// Text from the first line containing the guardrails heading (inclusive) to
/// end of file. Empty when the heading is absent, so the clause checks below
/// fail closed instead of matching prose anywhere else in the file.
fn guardrails_section(content: &str) -> String {
    let mut found = false;
    let mut lines = Vec::new();
    for line in content.split('\n') {
        if !found && line.contains(GUARDRAILS_HEADING) {
            found = true;
        }
        if found {
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// The heading, and the no-dash, grounding, and attribution clauses, all
/// matched case-insensitively but only inside the guardrails section.
/// Operates on the whole file text, not the frontmatter body.
fn check_guardrails(file: &str, content: &str) -> Vec<String> {
    let mut violations = Vec::new();
    if !content.contains(GUARDRAILS_HEADING) {
        violations.push(format!("{file}: missing '{GUARDRAILS_HEADING}' heading"));
    }

    let lower_section = guardrails_section(content).to_lowercase();
    let has_any = |needles: &[&str]| needles.iter().any(|n| lower_section.contains(n));

    if !has_any(&["no dashes", "em dash", "en dash"]) {
        violations.push(format!(
            "{file}: missing no-dash guardrail clause (no 'no dashes', 'em dash', or 'en dash' \
             found in the guardrails section)"
        ));
    }
    if !has_any(&["ground", "cite", "quote exact"]) {
        violations.push(format!(
            "{file}: missing grounding guardrail clause (no 'ground', 'cite', or 'quote exact' \
             found in the guardrails section)"
        ));
    }
    if !has_any(&["attribution"]) {
        violations.push(format!(
            "{file}: missing attribution guardrail clause (no 'attribution' found in the \
             guardrails section)"
        ));
    }

    violations
}

/// Run every rule against one agent definition's raw file content, recording
/// violations instead of stopping at the first one. `file` is the label used
/// in violation text (the path as passed in); `name` is the filename stem.
fn check_agent(file: &str, name: &str, content: &str) -> Vec<String> {
    let mut violations = Vec::new();

    match frontmatter_body(content) {
        Ok(body) => {
            violations.extend(check_required_keys(file, name, &body));
            violations.extend(check_tools(file, &body));
            violations.extend(check_yaml_scalars(file, &body));
        }
        Err(DelimiterError::MissingOpening) => {
            violations.push(format!("{file}: missing opening --- frontmatter delimiter"))
        }
        Err(DelimiterError::MissingClosing) => {
            violations.push(format!("{file}: missing closing --- frontmatter delimiter"))
        }
    }

    // Runs unconditionally, even when the frontmatter itself is unparsable:
    // check-agents.sh calls check_guardrails after, not inside, the
    // delimiter branch.
    violations.extend(check_guardrails(file, content));
    violations
}

/// The `agents/` directory of the enclosing repo's plugin folder (see
/// [`crate::plugin_root::in_checkout`]), used when no `AGENTS_DIR` argument
/// was given (check-agents.sh:20-25). Reuses
/// [`crate::manifest::check::toplevel`], since both validators resolve the
/// same repo root the same way. `None` outside a git repository.
pub fn default_dir() -> Option<PathBuf> {
    crate::manifest::check::toplevel()
        .map(|root| crate::plugin_root::in_checkout(&root).join("agents"))
}

/// Runs every rule against every `agents_dir/*.md` file (excluding
/// `_TEMPLATE.md`), mirroring `check-agents.sh`'s directory loop: success
/// reports the definition total, failure lists every offender.
pub fn check(agents_dir: &Path) -> Result<String, String> {
    if !agents_dir.is_dir() {
        return Err(format!(
            "agents directory not found: {}",
            agents_dir.display()
        ));
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(agents_dir)
        .map_err(|err| format!("failed to read agents directory: {err}"))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort();

    let mut violations = Vec::new();
    let mut count = 0usize;
    for path in entries {
        let base = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if base == TEMPLATE_NAME {
            continue;
        }
        count += 1;
        let name = base.strip_suffix(".md").unwrap_or(&base);
        let label = path.display().to_string();
        // An unreadable file degrades to empty content instead of aborting
        // the run: it then fails the opening-delimiter and guardrails
        // checks like any other malformed file, rather than panicking.
        let content = fs::read_to_string(&path).unwrap_or_default();
        violations.extend(check_agent(&label, name, &content));
    }

    if !violations.is_empty() {
        let mut message = format!(
            "{} violation(s) across agent definitions:",
            violations.len()
        );
        for v in &violations {
            message.push_str(&format!("\n  {v}"));
        }
        return Err(message);
    }

    Ok(format!(
        "check-agents: OK ({count} agent definitions, all valid)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_BODY: &str = "name: sample\ndescription: A structurally read-only fixture.\ntools: Read, Grep, Glob\nmodel: sonnet\neffort: medium";
    const GUARDRAILS_TAIL: &str = "\n## Non-negotiable guardrails\n\n1. No dashes in prose, no em dashes or en dashes.\n2. Ground every claim, quote exact code.\n3. Zero AI attribution.\n";

    #[test]
    fn frontmatter_body_delimiter_cases() {
        assert_eq!(
            frontmatter_body("---\nname: sample\n---\nbody text\n").ok(),
            Some("name: sample".to_string())
        );
        assert!(matches!(
            frontmatter_body("no delimiter here\n---\n"),
            Err(DelimiterError::MissingOpening)
        ));
        assert!(matches!(
            frontmatter_body("---\nname: sample\nno closing delimiter\n"),
            Err(DelimiterError::MissingClosing)
        ));
        // The shell's `[[ "$first_line" == "---" ]]` is an exact match; a
        // trailing space does not satisfy it either.
        assert!(matches!(
            frontmatter_body("--- \nname: sample\n---\n"),
            Err(DelimiterError::MissingOpening)
        ));
    }

    #[test]
    fn frontmatter_value_reads_first_match_and_strips_only_double_quotes() {
        assert_eq!(
            frontmatter_value(VALID_BODY, "name").as_deref(),
            Some("sample")
        );
        assert_eq!(frontmatter_value(VALID_BODY, "missing"), None);

        let body = "description: \"a: b\"\nother: 'a: b'";
        assert_eq!(
            frontmatter_value(body, "description").as_deref(),
            Some("a: b")
        );
        // Single quotes are NOT unwrapped, matching the shell's literal '"'
        // comparison.
        assert_eq!(frontmatter_value(body, "other").as_deref(), Some("'a: b'"));
    }

    #[test]
    fn required_keys_valid_body_has_no_violations() {
        assert!(check_required_keys("f.md", "sample", VALID_BODY).is_empty());
    }

    #[test]
    fn required_keys_name_mismatch_and_missing_keys_are_reported() {
        let mismatch = check_required_keys(
            "f.md",
            "sample",
            "name: not-sample\ndescription: x\ntools: Read\nmodel: sonnet\neffort: low",
        );
        assert_eq!(mismatch.len(), 1);
        assert!(mismatch[0].contains("does not match filename"));

        let missing = check_required_keys("f.md", "sample", "description: x\ntools: Read");
        for key in ["name", "model", "effort"] {
            assert!(missing
                .iter()
                .any(|v| v.contains(&format!("missing required frontmatter key '{key}'"))));
        }
    }

    #[test]
    fn required_keys_bad_model_and_effort_are_reported_with_the_allowed_list() {
        let bad_model = check_required_keys(
            "f.md",
            "sample",
            "name: sample\ndescription: x\ntools: Read\nmodel: gpt\neffort: low",
        );
        assert!(bad_model
            .iter()
            .any(|v| v.contains("model 'gpt' is not one of: haiku sonnet opus")));

        let bad_effort = check_required_keys(
            "f.md",
            "sample",
            "name: sample\ndescription: x\ntools: Read\nmodel: sonnet\neffort: extreme",
        );
        assert!(bad_effort
            .iter()
            .any(|v| v.contains("effort 'extreme' is not one of: low medium high xhigh max")));
    }

    #[test]
    fn tools_unknown_name_and_missing_key_cases() {
        let unknown = check_tools("f.md", "description: x\ntools: Read, Grepp, Glob");
        assert!(unknown.iter().any(|v| v.contains("Grepp")));
        assert!(check_tools("f.md", "description: x").is_empty());
    }

    #[test]
    fn tools_strict_tier_forbids_write_and_bash() {
        let violations = check_tools(
            "f.md",
            "description: A structurally read-only agent.\ntools: Read, Write, Bash",
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("or Bash, found: Write, Bash"));
    }

    #[test]
    fn tools_loose_tier_allows_bash_but_forbids_write() {
        assert!(check_tools(
            "f.md",
            "description: An isolated read-only agent.\ntools: Bash, Read"
        )
        .is_empty());

        let failing = check_tools(
            "f.md",
            "description: An isolated read-only agent.\ntools: Bash, Write, Read",
        );
        assert_eq!(failing.len(), 1);
        assert!(failing[0].contains("or NotebookEdit, found: Write"));
    }

    #[test]
    fn tools_write_capable_agent_without_read_only_claim_passes() {
        let body = "description: A write-capable executor.\ntools: Read, Edit, Write, Bash";
        assert!(check_tools("f.md", body).is_empty());
    }

    #[test]
    fn yaml_scalars_unquoted_colon_space_fails_quoted_and_bare_colon_pass() {
        let unquoted = check_yaml_scalars("f.md", "description: a prompt: a lens");
        assert_eq!(unquoted.len(), 1);
        assert!(unquoted[0].contains("colon-space"));

        assert!(check_yaml_scalars("f.md", "description: \"a prompt: a lens\"").is_empty());
        // "/playbook:deep-review" has a colon but no colon-SPACE.
        assert!(check_yaml_scalars("f.md", "description: see /playbook:deep-review").is_empty());
    }

    #[test]
    fn guardrails_all_clauses_present_has_no_violations() {
        let content = format!("intro\n{GUARDRAILS_TAIL}");
        assert!(check_guardrails("f.md", &content).is_empty());
    }

    #[test]
    fn guardrails_missing_heading_fails_closed_on_every_clause() {
        // No heading at all: the section is empty, so heading, no-dash,
        // grounding, and attribution all fail together.
        let violations = check_guardrails("f.md", "intro only, no guardrails section anywhere");
        assert_eq!(violations.len(), 4);
        assert!(violations[0].contains("missing '## Non-negotiable guardrails' heading"));
    }

    #[test]
    fn guardrails_single_missing_clause_cases() {
        // "em dash" only appears in the intro, above the heading, not inside
        // the section: pins that an unscoped match would wrongly pass.
        let outside = check_guardrails("f.md", "This intro names em dash on purpose.\n\n## Non-negotiable guardrails\n\n1. Ground every claim, quote exact code.\n2. Zero AI attribution.\n");
        assert_eq!(outside.len(), 1);
        assert!(outside[0].contains("missing no-dash guardrail clause"));

        let missing_grounding = check_guardrails("f.md", "intro\n\n## Non-negotiable guardrails\n\n1. No dashes, no em dash, no en dash.\n2. Zero AI attribution.\n");
        assert_eq!(missing_grounding.len(), 1);
        assert!(missing_grounding[0].contains("missing grounding guardrail clause"));
    }

    #[test]
    fn check_agent_well_formed_definition_has_no_violations() {
        let content = format!(
            "---\nname: sample\ndescription: A structurally read-only fixture.\ntools: Read, Grep, Glob\nmodel: sonnet\neffort: medium\n---\n\nbody text.{GUARDRAILS_TAIL}"
        );
        assert!(check_agent("f.md", "sample", &content).is_empty());
    }

    #[test]
    fn check_agent_bad_delimiters_still_run_the_guardrails_check() {
        let missing_open = check_agent(
            "f.md",
            "sample",
            &format!("no frontmatter at all.{GUARDRAILS_TAIL}"),
        );
        assert_eq!(missing_open.len(), 1);
        assert!(missing_open[0].contains("missing opening --- frontmatter delimiter"));

        let missing_close = check_agent(
            "f.md",
            "sample",
            &format!("---\nname: sample\nno closing delimiter{GUARDRAILS_TAIL}"),
        );
        assert!(missing_close
            .iter()
            .any(|v| v.contains("missing closing --- frontmatter delimiter")));
    }
}
