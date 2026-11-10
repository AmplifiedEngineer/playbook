// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Behavioural tests for the `rebuild-memory-graph` hook, the sole writer
//! of `~/.config/playbook/data/memory.db`. Exercised black-box, through the
//! compiled `playbook` binary, the same way
//! the retired shell test exercises the python original.
//! Every assertion in that shell script has a corresponding case below,
//! plus a comparison against the python implementation itself.
//!
//! Each test gets its own scratch `HOME` (never the real `~/.claude`) and
//! invokes the binary as a subprocess with `HOME` and `HOOK_INPUT` set on
//! the child only, so tests stay parallel-safe: no test here mutates this
//! process's own environment.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

// --- Test infrastructure ---------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory, with `.config/playbook/memory` already created, unique
/// per call so parallel test threads never collide.
fn scratch_home(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "playbook-wu5-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(home.join(".config").join("playbook").join("memory")).unwrap();
    home
}

/// Write `content` to `<home>/.config/playbook/memory/<relpath>`, creating parent
/// directories as needed.
fn write_fact(home: &Path, relpath: &str, content: &str) {
    let full = home
        .join(".config")
        .join("playbook")
        .join("memory")
        .join(relpath);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, content).unwrap();
}

/// The database the hook writes the graph to.
fn graph_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("playbook")
        .join("data")
        .join("memory.db")
}

/// The legacy `memory.graph.json` file of an older playbook.
fn legacy_json_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("playbook")
        .join("memory")
        .join("memory.graph.json")
}

/// The graph in `memory.db`, as the JSON document older versions kept in a file.
fn read_graph(home: &Path) -> Value {
    let root = home.join(".config").join("playbook");
    let content = playbook::hooks::rebuild_memory_graph::export_json(&root)
        .expect("memory.db should be readable")
        .expect("memory.db should exist");
    serde_json::from_str(&content).expect("the exported graph should be valid JSON")
}

fn run_playbook(home: &Path, args: &[&str], hook_input: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .args(args)
        .current_dir(env!("PLAYBOOK_REPO_ROOT"))
        .env("HOME", home)
        .env("HOOK_INPUT", hook_input)
        .stdin(Stdio::null())
        .output()
        .expect("playbook binary should run")
}

/// Run the rebuild-memory-graph hook with a synthetic `tool_input.file_path`
/// under `<home>/.config/playbook/memory/<relpath>`.
fn run_rebuild_for(home: &Path, relpath: &str) -> Output {
    let file_path = home
        .join(".config")
        .join("playbook")
        .join("memory")
        .join(relpath);
    run_rebuild_for_path(home, &file_path.to_string_lossy())
}

/// Same as `run_rebuild_for`, but with an already-built absolute path
/// (possibly outside the memory dir).
fn run_rebuild_for_path(home: &Path, file_path: &str) -> Output {
    let hook_input = json!({"tool_input": {"file_path": file_path}}).to_string();
    run_playbook(home, &["hook", "rebuild-memory-graph"], &hook_input)
}

fn nodes(graph: &Value) -> &Vec<Value> {
    graph["nodes"].as_array().expect("nodes should be an array")
}

fn edges(graph: &Value) -> &Vec<Value> {
    graph["edges"].as_array().expect("edges should be an array")
}

fn has_node(graph: &Value, id: &str) -> bool {
    nodes(graph).iter().any(|n| n["id"] == id)
}

fn has_edge(graph: &Value, from: &str, to: &str, relation: &str) -> bool {
    edges(graph)
        .iter()
        .any(|e| e["from"] == from && e["to"] == to && e["relation"] == relation)
}

/// Find the `signals` array of the `possible_relates_to` edge between `a`
/// and `b`, checked in either direction since which of the two facts ends
/// up as `from` depends on filesystem walk order, which this suite does not
/// control.
fn possible_relates_to_signals<'a>(graph: &'a Value, a: &str, b: &str) -> Option<&'a Vec<Value>> {
    edges(graph)
        .iter()
        .find(|e| {
            e["relation"] == "possible_relates_to"
                && ((e["from"] == a && e["to"] == b) || (e["from"] == b && e["to"] == a))
        })
        .and_then(|e| e["signals"].as_array())
}

// --- rebuild-memory-graph: mandatory frontmatter shape matrix --------------
//
// (1) top-level scalars, (2) block lists, (3) nested dict sub-keys,
// (4) inline sequences, (5) a dangling edge target, (6) a supersedes chain
// two links long. Every scenario below is annotated with which shape(s) it
// exercises.

/// Shape (1): a plain top-level scalar frontmatter field (`description`),
/// checked explicitly since the shell suite only ever exercises `name`/
/// `type` scalars incidentally.
#[test]
fn top_level_scalar_description_flows_into_the_node() {
    // Arrange
    let home = scratch_home("scalar-description");
    write_fact(
        &home,
        "described-fact.md",
        "---\nname: described-fact\ntype: reference\ndescription: A fact with an explicit description.\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "described-fact.md");

    // Assert
    let graph = read_graph(&home);
    let node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "global/described-fact")
        .expect("node should exist");
    assert_eq!(node["description"], "A fact with an explicit description.");

    let _ = fs::remove_dir_all(&home);
}

/// Additive field: `pinned: true` in frontmatter round-trips onto the node
/// as `"pinned":true`.
#[test]
fn pinned_true_frontmatter_flows_into_the_node() {
    // Arrange
    let home = scratch_home("pinned-true");
    write_fact(
        &home,
        "pinned-fact.md",
        "---\nname: pinned-fact\ntype: reference\npinned: true\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "pinned-fact.md");

    // Assert
    let graph = read_graph(&home);
    let node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "global/pinned-fact")
        .expect("node should exist");
    assert_eq!(node["pinned"], true);

    let _ = fs::remove_dir_all(&home);
}

/// Additive field: a fact with no `pinned` key at all omits the field from
/// the output JSON entirely, never `"pinned":false`. Checked against the raw
/// JSON `Value`'s object keys, not a Rust-side `Option`, since the point of
/// `skip_serializing_if` is what actually lands in the file.
#[test]
fn pinned_absent_frontmatter_omits_the_node_field() {
    // Arrange
    let home = scratch_home("pinned-absent");
    write_fact(
        &home,
        "unpinned-fact.md",
        "---\nname: unpinned-fact\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "unpinned-fact.md");

    // Assert
    let graph = read_graph(&home);
    let node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "global/unpinned-fact")
        .expect("node should exist");
    assert!(
        !node.as_object().unwrap().contains_key("pinned"),
        "a fact with no pinned key should omit the field, not write pinned:false"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Additive field: `pinned: false` omits the field too, same as absent. This
/// pins the parser's exact match against the literal `true`, not a general
/// boolean parse, as a deliberate contract rather than an untested side effect.
#[test]
fn pinned_false_frontmatter_omits_the_node_field() {
    // Arrange
    let home = scratch_home("pinned-false");
    write_fact(
        &home,
        "unpinned-fact.md",
        "---\nname: unpinned-fact\ntype: reference\npinned: false\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "unpinned-fact.md");

    // Assert
    let graph = read_graph(&home);
    let node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "global/unpinned-fact")
        .expect("node should exist");
    assert!(
        !node.as_object().unwrap().contains_key("pinned"),
        "pinned: false should omit the field, not write pinned:false"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Additive field: a value other than the literal `true` (quoted, a different
/// case, or a YAML-style yes) is not matched and omits the field, same as
/// absent. Pins the exact-match contract against a value that isn't a clean
/// boolean.
#[test]
fn pinned_non_literal_true_value_omits_the_node_field() {
    // Arrange
    let home = scratch_home("pinned-non-literal");
    write_fact(
        &home,
        "unpinned-fact.md",
        "---\nname: unpinned-fact\ntype: reference\npinned: yes\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "unpinned-fact.md");

    // Assert
    let graph = read_graph(&home);
    let node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "global/unpinned-fact")
        .expect("node should exist");
    assert!(
        !node.as_object().unwrap().contains_key("pinned"),
        "a non-literal-true pinned value should omit the field, not match loosely"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Shape (3): a dict sub-key (`links.relates_to`) holding a bare scalar.
/// the retired shell test scenario 1.
#[test]
fn scalar_link_produces_one_edge() {
    // Arrange
    let home = scratch_home("scalar-link");
    write_fact(
        &home,
        "scalar-fact.md",
        "---\nname: scalar-fact\ntype: reference\nlinks:\n  relates_to: other-fact\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "scalar-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 1);
    assert_eq!(edges(&graph)[0]["from"], "global/scalar-fact");
    assert_eq!(edges(&graph)[0]["to"], "global/other-fact");
    assert_eq!(edges(&graph)[0]["relation"], "relates_to");

    let _ = fs::remove_dir_all(&home);
}

/// Shape (4): an inline flow sequence `[a, b, c]` under a dict sub-key.
/// the retired shell test scenario 2.
#[test]
fn inline_list_produces_one_edge_per_target() {
    // Arrange
    let home = scratch_home("inline-list");
    write_fact(
        &home,
        "list-fact.md",
        "---\nname: list-fact\ntype: reference\nlinks:\n  relates_to: [alpha, beta, gamma]\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "list-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 3);
    for target in ["alpha", "beta", "gamma"] {
        assert!(
            has_edge(
                &graph,
                "global/list-fact",
                &format!("global/{target}"),
                "relates_to"
            ),
            "expected edge to global/{target}"
        );
    }

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 3: a single-element inline
/// list produces one edge whose target id carries no bracket characters.
#[test]
fn single_element_inline_list_has_no_brackets_in_target_id() {
    // Arrange
    let home = scratch_home("inline-list-single");
    write_fact(
        &home,
        "single-fact.md",
        "---\nname: single-fact\ntype: reference\nlinks:\n  relates_to: [solo]\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "single-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 1);
    assert_eq!(edges(&graph)[0]["to"], "global/solo");

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 4: quoted inline items parse
/// to clean, unquoted names.
#[test]
fn quoted_inline_items_parse_to_clean_names() {
    // Arrange
    let home = scratch_home("inline-list-quoted");
    write_fact(
        &home,
        "quoted-fact.md",
        "---\nname: quoted-fact\ntype: reference\nlinks:\n  relates_to: [\"quoted-a\", 'quoted-b']\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "quoted-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 2);
    assert!(has_edge(
        &graph,
        "global/quoted-fact",
        "global/quoted-a",
        "relates_to"
    ));
    assert!(has_edge(
        &graph,
        "global/quoted-fact",
        "global/quoted-b",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 5: an empty inline list
/// produces no edges and does not crash the hook.
#[test]
fn empty_inline_list_produces_no_edges() {
    // Arrange
    let home = scratch_home("inline-list-empty");
    write_fact(
        &home,
        "empty-list-fact.md",
        "---\nname: empty-list-fact\ntype: reference\nlinks:\n  relates_to: []\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "empty-list-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 0);
    assert_eq!(nodes(&graph).len(), 1);

    let _ = fs::remove_dir_all(&home);
}

/// Shape (2)/(3): a block list nested under a dict sub-key.
/// the retired shell test scenario 6.
#[test]
fn nested_block_list_produces_one_edge_per_item() {
    // Arrange
    let home = scratch_home("nested-block-list");
    write_fact(
        &home,
        "nested-fact.md",
        "---\nname: nested-fact\ntype: reference\nlinks:\n  relates_to:\n    - item-one\n    - item-two\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "nested-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 2);
    assert!(has_edge(
        &graph,
        "global/nested-fact",
        "global/item-one",
        "relates_to"
    ));
    assert!(has_edge(
        &graph,
        "global/nested-fact",
        "global/item-two",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// Shape (2): a top-level block list (`anchors`), unrelated to the `links`
/// dict machinery. the retired shell test scenario 7.
#[test]
fn anchors_produce_code_nodes_and_anchors_edges() {
    // Arrange
    let home = scratch_home("anchors-block-list");
    write_fact(
        &home,
        "anchor-fact.md",
        "---\nname: anchor-fact\ntype: reference\nanchors:\n  - src/index.ts\n  - src/other.ts\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "anchor-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 2);
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "code:src/index.ts" && n["type"] == "code"));
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "code:src/other.ts" && n["type"] == "code"));
    assert!(has_edge(
        &graph,
        "global/anchor-fact",
        "code:src/index.ts",
        "anchors"
    ));
    assert!(has_edge(
        &graph,
        "global/anchor-fact",
        "code:src/other.ts",
        "anchors"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// Shape (2) inline variant, WU-5 item 1: a top-level inline flow sequence
/// (`anchors: [a, b]`) must produce code nodes and `anchors` edges exactly
/// like the block-style form above. Before the fix, the top-level branch of
/// the frontmatter parser stored the raw bracketed string instead of
/// routing it through the inline-list parser: python then iterated that
/// string one character at a time (junk nodes), and Rust looked the value
/// up in its list map, found nothing, and silently emitted no anchors.
#[test]
fn inline_top_level_anchors_produce_code_nodes_and_anchors_edges() {
    // Arrange
    let home = scratch_home("anchors-inline-list");
    write_fact(
        &home,
        "inline-anchor-fact.md",
        "---\nname: inline-anchor-fact\ntype: reference\nanchors: [src/index.ts, src/other.ts]\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "inline-anchor-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 2);
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "code:src/index.ts" && n["type"] == "code"));
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "code:src/other.ts" && n["type"] == "code"));
    assert!(has_edge(
        &graph,
        "global/inline-anchor-fact",
        "code:src/index.ts",
        "anchors"
    ));
    assert!(has_edge(
        &graph,
        "global/inline-anchor-fact",
        "code:src/other.ts",
        "anchors"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// WU-5 item 1, the property that was silently broken: an inline-style
/// top-level `anchors:` fact and a block-style top-level `anchors:` fact
/// naming the same paths must produce the identical set of code nodes and
/// `anchors` edges, differing only in which fact each edge originates from.
#[test]
fn inline_and_block_style_anchors_produce_the_same_code_nodes_and_edges() {
    // Arrange
    let home = scratch_home("anchors-inline-vs-block");
    write_fact(
        &home,
        "inline-style-fact.md",
        "---\nname: inline-style-fact\ntype: reference\nanchors: [src/shared.ts, src/other.ts]\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "block-style-fact.md",
        "---\nname: block-style-fact\ntype: reference\nanchors:\n  - src/shared.ts\n  - src/other.ts\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "inline-style-fact.md");

    // Assert
    let graph = read_graph(&home);
    // Both facts anchor the same two paths, so the code nodes are shared
    // (deduplicated by id) and each fact contributes one anchors edge per
    // path: four edges total, but only two code nodes. The two facts also
    // anchor the same parent directory, share a type and scope, and share a
    // body, so the pairwise similarity check adds a fifth edge between them.
    assert_eq!(edges(&graph).len(), 5);
    assert!(
        possible_relates_to_signals(&graph, "global/inline-style-fact", "global/block-style-fact")
            .is_some(),
        "two facts anchoring the same directory with a matching type, scope, and body should get a possible_relates_to edge"
    );
    for path in ["src/shared.ts", "src/other.ts"] {
        let cid = format!("code:{path}");
        assert_eq!(
            nodes(&graph).iter().filter(|n| n["id"] == cid).count(),
            1,
            "code node for {path} should be deduplicated across both facts"
        );
        assert!(
            has_edge(&graph, "global/inline-style-fact", &cid, "anchors"),
            "inline-style fact should anchor {path}"
        );
        assert!(
            has_edge(&graph, "global/block-style-fact", &cid, "anchors"),
            "block-style fact should anchor {path}"
        );
    }

    let _ = fs::remove_dir_all(&home);
}

/// Shape (5): a dangling target is surfaced as an edge, not dropped.
/// the retired shell test scenario 8.
#[test]
fn dangling_target_still_emits_its_edge() {
    // Arrange
    let home = scratch_home("dangling-global");
    write_fact(
        &home,
        "dangling-fact.md",
        "---\nname: dangling-fact\ntype: reference\nlinks:\n  relates_to: does-not-exist\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "dangling-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 1);
    assert_eq!(edges(&graph)[0]["to"], "global/does-not-exist");
    assert!(
        !has_node(&graph, "global/does-not-exist"),
        "dangling target node should not exist"
    );

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 9: a project-scoped fact
/// gets an `owner/repo/name` id, a global fact gets `global/name`, and a
/// project-scoped edge target keeps the owner/repo prefix.
#[test]
fn project_scoped_and_global_facts_get_distinct_ids() {
    // Arrange
    let home = scratch_home("scope-ids");
    write_fact(
        &home,
        "acme/widget/proj-fact.md",
        "---\nname: proj-fact\ntype: reference\nlinks:\n  relates_to: sibling-fact\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "global-fact.md",
        "---\nname: global-fact\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/widget/proj-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "acme/widget/proj-fact" && n["project"] == "acme/widget"));
    assert!(has_node(&graph, "global/global-fact"));
    assert!(has_edge(
        &graph,
        "acme/widget/proj-fact",
        "acme/widget/sibling-fact",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn org_scoped_facts_get_a_distinct_scope_and_id() {
    // Arrange: a two-segment path (owner/fact.md), alongside the existing one- and three-segment shapes.
    let home = scratch_home("org-scope-ids");
    write_fact(
        &home,
        "acme/org-fact.md",
        "---\nname: org-fact\ntype: reference\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "acme/widget/proj-fact.md",
        "---\nname: proj-fact\ntype: reference\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "global-fact.md",
        "---\nname: global-fact\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/org-fact.md");

    // Assert
    let graph = read_graph(&home);
    let org_node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "acme/org-fact")
        .expect("org-scoped node should exist");
    assert_eq!(org_node["scope"], "org");
    assert_eq!(org_node["project"], "acme");
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "acme/widget/proj-fact" && n["scope"] == "project"));
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "global/global-fact" && n["scope"] == "global"));

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn org_source_resolves_a_link_in_its_own_scope_before_global() {
    // Arrange: an org-scoped source links to a same-named target in both its own org scope and globally.
    let home = scratch_home("org-link-own-scope");
    write_fact(
        &home,
        "acme/org-source.md",
        "---\nname: org-source\ntype: reference\nlinks:\n  relates_to: [dup]\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "acme/dup.md",
        "---\nname: dup\ntype: reference\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "dup.md",
        "---\nname: dup\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/org-source.md");

    // Assert
    let graph = read_graph(&home);
    assert!(has_edge(
        &graph,
        "acme/org-source",
        "acme/dup",
        "relates_to"
    ));
    assert!(!has_edge(
        &graph,
        "acme/org-source",
        "global/dup",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 10 and the brief's "non-memory
/// file edits are a no-op" done-when criterion: writing a file outside the
/// memory dir leaves memory.db completely untouched, even when a second
/// in-scope fact was added to disk (but not via a hook-triggering write)
/// in between.
#[test]
fn outside_memory_dir_write_is_a_no_op() {
    // Arrange
    let home = scratch_home("outside-write");
    write_fact(
        &home,
        "out-fact-1.md",
        "---\nname: out-fact-1\ntype: reference\n---\n\nBody text.\n",
    );
    run_rebuild_for(&home, "out-fact-1.md");
    let baseline = read_graph(&home);
    assert_eq!(
        nodes(&baseline).len(),
        1,
        "baseline graph should have one node"
    );
    write_fact(
        &home,
        "out-fact-2.md",
        "---\nname: out-fact-2\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    let outside_path = home.join("outside-memory-dir.md");
    run_rebuild_for_path(&home, &outside_path.to_string_lossy());

    // Assert
    let after = read_graph(&home);
    assert_eq!(
        nodes(&after).len(),
        1,
        "graph should be untouched; the second fact must not be picked up"
    );

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 11: malformed or absent
/// frontmatter does not crash the hook; the fact still gets a node built
/// from filename-derived defaults.
#[test]
fn malformed_or_absent_frontmatter_falls_back_to_defaults() {
    // Arrange
    let home = scratch_home("malformed-frontmatter");
    write_fact(
        &home,
        "no-frontmatter.md",
        "Just a note with no frontmatter at all.\n",
    );
    write_fact(
        &home,
        "malformed-frontmatter.md",
        "---\nname: malformed\nThis has no closing delimiter.\n",
    );

    // Act
    let output = run_rebuild_for(&home, "no-frontmatter.md");

    // Assert
    assert_eq!(output.status.code(), Some(0), "hook should exit cleanly");
    let graph = read_graph(&home); // valid JSON, or this would already have failed
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "global/no-frontmatter"
            && n["type"] == "reference"
            && n["name"] == "no-frontmatter"));
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "global/malformed-frontmatter" && n["type"] == "reference"));

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 12: a project-scoped fact
/// linking to a global fact resolves cross-scope.
#[test]
fn project_fact_resolves_a_link_to_a_global_target() {
    // Arrange
    let home = scratch_home("cross-scope-resolve");
    write_fact(
        &home,
        "acme/widget/local-fact.md",
        "---\nname: local-fact\ntype: reference\nlinks:\n  relates_to: [global-thing]\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "global-thing.md",
        "---\nname: global-thing\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/widget/local-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 1);
    assert!(has_edge(
        &graph,
        "acme/widget/local-fact",
        "global/global-thing",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 13: same-scope-then-global
/// fallback prefers a same-named fact in the source's own project scope
/// over a same-named global fact.
#[test]
fn own_scope_wins_over_a_same_named_global_fact() {
    // Arrange
    let home = scratch_home("own-scope-wins");
    write_fact(
        &home,
        "acme/widget/proj-source.md",
        "---\nname: proj-source\ntype: reference\nlinks:\n  relates_to: [dup]\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "acme/widget/dup.md",
        "---\nname: dup\ntype: reference\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "dup.md",
        "---\nname: dup\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/widget/proj-source.md");

    // Assert
    let graph = read_graph(&home);
    assert!(has_edge(
        &graph,
        "acme/widget/proj-source",
        "acme/widget/dup",
        "relates_to"
    ));
    assert!(!has_edge(
        &graph,
        "acme/widget/proj-source",
        "global/dup",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// Shape (5) in project scope: the retired shell test scenario
/// 14, a project link to a target that exists nowhere still dangles using
/// the same-scope id.
#[test]
fn project_scoped_dangling_target_uses_same_scope_id() {
    // Arrange
    let home = scratch_home("dangling-project");
    write_fact(
        &home,
        "acme/widget/missing-source.md",
        "---\nname: missing-source\ntype: reference\nlinks:\n  relates_to: nope\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/widget/missing-source.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 1);
    assert_eq!(edges(&graph)[0]["to"], "acme/widget/nope");
    assert!(!has_node(&graph, "acme/widget/nope"));

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 15: a global source resolves
/// in the global scope, unaffected by the two-pass project resolution.
///
/// The two facts' bodies are deliberately distinct (not the usual "Body
/// text." placeholder shared by most fixtures in this file): both are
/// `type: reference` at the same (global) scope, so an identical body would
/// also satisfy the pairwise similarity check added for
/// `possible_relates_to` edges, adding a second edge this test does not
/// expect.
#[test]
fn global_source_is_unaffected_by_project_scope_resolution() {
    // Arrange
    let home = scratch_home("global-source-unaffected");
    write_fact(
        &home,
        "global-source.md",
        "---\nname: global-source\ntype: reference\nlinks:\n  relates_to: global-target\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "global-target.md",
        "---\nname: global-target\ntype: reference\n---\n\nUnrelated placeholder content only.\n",
    );

    // Act
    run_rebuild_for(&home, "global-source.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 1);
    assert_eq!(edges(&graph)[0]["from"], "global/global-source");
    assert_eq!(edges(&graph)[0]["to"], "global/global-target");

    let _ = fs::remove_dir_all(&home);
}

/// the retired shell test scenario 16: `anchors` and `links` on
/// the same fact do not interfere with each other.
///
/// `combo-target`'s body is deliberately distinct from `combo-fact`'s (not
/// the usual "Body text." placeholder): both are `type: reference` at the
/// same (project) scope, so an identical body would also satisfy the
/// pairwise similarity check added for `possible_relates_to` edges, adding
/// a third edge this test does not expect.
#[test]
fn anchors_and_links_on_the_same_fact_are_independent() {
    // Arrange
    let home = scratch_home("anchors-and-links-combo");
    write_fact(
        &home,
        "acme/widget/combo-fact.md",
        "---\nname: combo-fact\ntype: reference\nlinks:\n  relates_to: [combo-target]\nanchors:\n  - src/combo.ts\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "acme/widget/combo-target.md",
        "---\nname: combo-target\ntype: reference\n---\n\nUnrelated placeholder content only.\n",
    );

    // Act
    run_rebuild_for(&home, "acme/widget/combo-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(edges(&graph).len(), 2);
    assert!(nodes(&graph)
        .iter()
        .any(|n| n["id"] == "code:acme/widget/src/combo.ts"
            && n["type"] == "code"
            && n["project"] == "acme/widget"));
    assert!(has_edge(
        &graph,
        "acme/widget/combo-fact",
        "code:acme/widget/src/combo.ts",
        "anchors"
    ));
    assert!(has_edge(
        &graph,
        "acme/widget/combo-fact",
        "acme/widget/combo-target",
        "relates_to"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// Shape (6): a `supersedes` chain two links long (fact-v1 supersedes
/// fact-v2 supersedes fact-v3), verifying pass-2 edge resolution generalises
/// to an arbitrary relation name and to a node that is both a link source
/// and, transitively, part of another link's chain. Not in the shell suite;
/// required explicitly by the WU-5 brief's fixture matrix.
#[test]
fn supersedes_chain_two_links_long_resolves_both_hops() {
    // Arrange
    let home = scratch_home("supersedes-chain");
    write_fact(
        &home,
        "fact-v1.md",
        "---\nname: fact-v1\ntype: reference\nlinks:\n  supersedes: fact-v2\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "fact-v2.md",
        "---\nname: fact-v2\ntype: reference\nlinks:\n  supersedes: fact-v3\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "fact-v3.md",
        "---\nname: fact-v3\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "fact-v1.md");

    // Assert
    let graph = read_graph(&home);
    assert!(has_node(&graph, "global/fact-v1"));
    assert!(has_node(&graph, "global/fact-v2"));
    assert!(has_node(&graph, "global/fact-v3"));
    assert!(has_edge(
        &graph,
        "global/fact-v1",
        "global/fact-v2",
        "supersedes"
    ));
    assert!(has_edge(
        &graph,
        "global/fact-v2",
        "global/fact-v3",
        "supersedes"
    ));

    let _ = fs::remove_dir_all(&home);
}

/// WU-5 item 2: a later top-level redeclaration of a key evicts whatever
/// shape (scalar, list, or dict) the earlier declaration held. Here
/// `links.relates_to: a` builds a dict, then the plain scalar `links:
/// not-a-dict` redeclares the same top-level key and must fully replace it,
/// so no edges come from the evicted dict.
#[test]
fn later_top_level_key_redeclaration_evicts_the_earlier_shape() {
    // Arrange
    let home = scratch_home("duplicate-key-shape-change");
    write_fact(
        &home,
        "duplicate-key-fact.md",
        "---\nname: duplicate-key-fact\ntype: reference\nlinks:\n  relates_to: a\nlinks: not-a-dict\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "duplicate-key-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert_eq!(
        edges(&graph).len(),
        0,
        "the later scalar links: redeclaration should evict the earlier dict, leaving zero edges"
    );

    let _ = fs::remove_dir_all(&home);
}

/// WU-5 item 3: a bare `\r` (not part of a `\r\n` pair) inside a
/// frontmatter scalar value must not corrupt it. The parser treats the bare
/// `\r` as a line break, matching python's `str.splitlines()`, so the
/// description ends at the `\r` and the trailing text after it is silently
/// ignored, exactly as python's frontmatter parser already does.
#[test]
fn bare_carriage_return_does_not_corrupt_a_scalar_value() {
    // Arrange
    let home = scratch_home("bare-cr-mid-frontmatter");
    write_fact(
        &home,
        "cr-fact.md",
        "---\nname: cr-fact\ntype: reference\ndescription: before\rafter\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "cr-fact.md");

    // Assert
    let graph = read_graph(&home);
    let node = nodes(&graph)
        .iter()
        .find(|n| n["id"] == "global/cr-fact")
        .expect("node should exist");
    assert_eq!(node["description"], "before");

    let _ = fs::remove_dir_all(&home);
}

/// Done-when: the graph write is atomic, so a write that cannot complete
/// (stood in for a crash mid-write by making the database read-only right
/// before the rebuild) leaves the
/// previously written graph intact rather than emptied.
#[test]
fn graph_write_is_atomic_a_failed_write_cannot_empty_the_existing_graph() {
    // Arrange
    let home = scratch_home("atomic-write");
    let db = graph_path(&home);
    write_fact(
        &home,
        "seed.md",
        "---\nname: seed\ntype: reference\n---\n\nBody.\n",
    );
    run_rebuild_for(&home, "seed.md"); // establishes a real memory.db while the dir is still writable

    if !permission_checks_are_enforced(db.parent().unwrap()) {
        eprintln!(
            "skipping graph_write_is_atomic_a_failed_write_cannot_empty_the_existing_graph: \
             this filesystem/user does not enforce permission bits (likely running as root)"
        );
        let _ = fs::remove_dir_all(&home);
        return;
    }

    write_fact(
        &home,
        "second.md",
        "---\nname: second\ntype: reference\n---\n\nBody.\n",
    );
    set_mode(&db, 0o444); // read only: the database cannot be written

    // Act
    run_rebuild_for(&home, "second.md");

    // Assert: the write failed before anything was stored, so the graph still
    // holds the seed alone.
    set_mode(&db, 0o644);
    let after = read_graph(&home);
    assert!(has_node(&after, "global/seed"));
    assert!(!has_node(&after, "global/second"));

    let _ = fs::remove_dir_all(&home);
}

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).unwrap();
}

/// Probe whether this process/filesystem actually enforces the write bit,
/// so the atomicity test above degrades to a skip (not a false failure)
/// when running as root, where permission bits are bypassed.
fn permission_checks_are_enforced(dir: &Path) -> bool {
    let probe = dir.join(".perm-probe");
    set_mode(dir, 0o555);
    let blocked = fs::write(&probe, "x").is_err();
    set_mode(dir, 0o755);
    let _ = fs::remove_file(&probe);
    blocked
}

/// Done-when: `playbook memory rebuild` (the CLI subcommand) fails loud when
/// a memory subdirectory cannot be read, instead of silently walking past it
/// and printing the hardcoded success line. The error must also name the
/// specific subdirectory that failed, not just the top-level memory root.
#[test]
fn memory_rebuild_subcommand_fails_loud_when_a_subdirectory_cannot_be_read() {
    // Arrange
    let home = scratch_home("unreadable-subdir-cli");
    let mem_dir = home.join(".config").join("playbook").join("memory");
    write_fact(
        &home,
        "seed.md",
        "---\nname: seed\ntype: reference\n---\n\nBody.\n",
    );
    let bad_subdir = mem_dir.join("no-access-subdir");
    fs::create_dir_all(&bad_subdir).unwrap();

    if !permission_checks_are_enforced(&mem_dir) {
        eprintln!(
            "skipping memory_rebuild_subcommand_fails_loud_when_a_subdirectory_cannot_be_read: \
             this filesystem/user does not enforce permission bits (likely running as root)"
        );
        let _ = fs::remove_dir_all(&home);
        return;
    }

    set_mode(&bad_subdir, 0o000);

    // Act
    let out = run_playbook(&home, &["memory", "rebuild"], "");

    // Assert
    set_mode(&bad_subdir, 0o755); // restore before cleanup can remove the dir
    assert!(
        !out.status.success(),
        "memory rebuild should exit non-zero when a subdirectory cannot be read"
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty when the rebuild fails"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("read memory directory"),
        "stderr should contain the RebuildError wrapper text: {stderr}"
    );
    assert!(
        stderr.contains("no-access-subdir"),
        "stderr should name the specific failing subdirectory: {stderr}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Done-when: the same unreadable-subdirectory failure, exercised via the
/// PostToolUse hook path instead of the CLI subcommand, keeps the hook's
/// documented exit-0/stdout-silent contract, but the failure is now
/// observable on stderr instead of vanishing.
#[test]
fn hook_rebuild_path_stays_silent_on_stdout_but_reports_unreadable_subdirectory_on_stderr() {
    // Arrange
    let home = scratch_home("unreadable-subdir-hook");
    let mem_dir = home.join(".config").join("playbook").join("memory");
    write_fact(
        &home,
        "seed.md",
        "---\nname: seed\ntype: reference\n---\n\nBody.\n",
    );
    let bad_subdir = mem_dir.join("no-access-subdir");
    fs::create_dir_all(&bad_subdir).unwrap();

    if !permission_checks_are_enforced(&mem_dir) {
        eprintln!(
            "skipping hook_rebuild_path_stays_silent_on_stdout_but_reports_unreadable_subdirectory_on_stderr: \
             this filesystem/user does not enforce permission bits (likely running as root)"
        );
        let _ = fs::remove_dir_all(&home);
        return;
    }

    set_mode(&bad_subdir, 0o000);

    // Act
    let out = run_rebuild_for(&home, "seed.md");

    // Assert
    set_mode(&bad_subdir, 0o755); // restore before cleanup can remove the dir
    assert!(
        out.status.success(),
        "the hook path must still exit 0 when a subdirectory cannot be read"
    );
    assert!(
        out.stdout.is_empty(),
        "the hook path must stay silent on stdout"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("read memory directory"),
        "stderr should contain the RebuildError wrapper text: {stderr}"
    );
    assert!(
        stderr.contains("no-access-subdir"),
        "stderr should name the specific failing subdirectory: {stderr}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Done-when: a write failure (the database is read-only, so it cannot be
/// written) makes `playbook memory rebuild` exit non-zero and
/// report the real failure on stderr, instead of the hardcoded success line,
/// and leaves the previously written graph intact.
#[test]
fn memory_rebuild_subcommand_fails_loud_on_a_write_failure() {
    // Arrange
    let home = scratch_home("write-failure-cli");
    let db = graph_path(&home);
    write_fact(
        &home,
        "seed.md",
        "---\nname: seed\ntype: reference\n---\n\nBody.\n",
    );
    run_rebuild_for(&home, "seed.md"); // establishes a real memory.db while the dir is still writable

    if !permission_checks_are_enforced(db.parent().unwrap()) {
        eprintln!(
            "skipping memory_rebuild_subcommand_fails_loud_on_a_write_failure: \
             this filesystem/user does not enforce permission bits (likely running as root)"
        );
        let _ = fs::remove_dir_all(&home);
        return;
    }

    let before = read_graph(&home);
    set_mode(&db, 0o444); // read only: the database cannot be written

    // Act
    let out = run_playbook(&home, &["memory", "rebuild"], "");

    // Assert
    set_mode(&db, 0o644);
    assert!(
        !out.status.success(),
        "memory rebuild should exit non-zero when the graph write fails"
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty when the rebuild fails"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("write memory.db"),
        "stderr should contain the RebuildError wrapper text: {stderr}"
    );
    assert_eq!(
        read_graph(&home),
        before,
        "the previous graph must stay intact after a failed write"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Done-when: a database that cannot be opened (the path is occupied by a
/// directory) makes `playbook memory rebuild` exit non-zero and report the
/// real failure on stderr. Forced deterministically by replacing the
/// database with a directory of the same name, not by permission bits, so
/// this needs no root-skip.
#[test]
fn memory_rebuild_subcommand_fails_loud_when_the_database_cannot_be_opened() {
    // Arrange
    let home = scratch_home("rename-failure-cli");
    write_fact(
        &home,
        "seed.md",
        "---\nname: seed\ntype: reference\n---\n\nBody.\n",
    );
    run_rebuild_for(&home, "seed.md"); // establishes a real memory.db

    fs::remove_file(graph_path(&home)).unwrap();
    fs::create_dir(graph_path(&home)).unwrap();

    // Act
    let out = run_playbook(&home, &["memory", "rebuild"], "");

    // Assert
    assert!(
        !out.status.success(),
        "memory rebuild should exit non-zero when the database cannot be opened"
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty when the rebuild fails"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("write memory.db"),
        "stderr should contain the RebuildError wrapper text: {stderr}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// A regression guard for the `.md` extension match: an upper-case
/// `.MD` suffix must still be picked up by the walk, not silently skipped,
/// and must not leak a literal `.MD`/`.md` tail into the derived node id or
/// default name.
#[test]
fn uppercase_md_extension_is_matched() {
    // Arrange
    let home = scratch_home("case-insensitive-md");
    write_fact(
        &home,
        "upper-fact.MD",
        "---\nname: upper-fact\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "upper-fact.MD");

    // Assert: node id derivation strips a `.MD` suffix the same as a
    // lowercase one, so a `.MD` fact gets a clean id, not one with the
    // literal suffix still attached.
    let graph = read_graph(&home);
    assert!(
        has_node(&graph, "global/upper-fact"),
        "a .MD-suffixed fact should get a clean node id, not one with .MD baked in"
    );

    let _ = fs::remove_dir_all(&home);
}

/// A fact whose filename mixes case in its `.md` suffix (`.Md`) and carries
/// no `name` frontmatter field falls back to the filename minus the
/// suffix; that fallback must also strip the suffix case-insensitively.
#[test]
fn mixed_case_md_extension_strips_cleanly_into_the_default_name() {
    // Arrange
    let home = scratch_home("case-insensitive-md-default-name");
    write_fact(
        &home,
        "no-name-field.Md",
        "---\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "no-name-field.Md");

    // Assert
    let graph = read_graph(&home);
    assert!(
        nodes(&graph).iter().any(|n| n["name"] == "no-name-field"),
        "the default-name fallback must strip a .Md suffix case-insensitively too"
    );

    let _ = fs::remove_dir_all(&home);
}

/// `MEMORY.md`, exact case, stays excluded from the walk even once the
/// `.md` extension match itself becomes case-insensitive; so does a
/// differently-cased `MEMORY.MD`, since the exclusion is case-insensitive too.
#[test]
fn exact_case_memory_md_is_still_excluded() {
    // Arrange
    let home = scratch_home("case-insensitive-md-exclusion");
    write_fact(&home, "MEMORY.md", "This file must never become a node.\n");
    write_fact(
        &home,
        "MEMORY.MD",
        "This file must never become a node either.\n",
    );
    write_fact(
        &home,
        "normal-fact.md",
        "---\nname: normal-fact\ntype: reference\n---\n\nBody text.\n",
    );

    // Act
    run_rebuild_for(&home, "normal-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert!(
        !has_node(&graph, "global/MEMORY"),
        "MEMORY.md must still be excluded from the graph"
    );
    assert_eq!(
        nodes(&graph).len(),
        1,
        "MEMORY.MD must be excluded too, leaving only normal-fact.md's node: {graph:?}"
    );
    assert!(has_node(&graph, "global/normal-fact"));

    let _ = fs::remove_dir_all(&home);
}

// --- Cross-implementation check --------------------------------------------

/// Populate an identical fixture memory tree, covering all six mandatory
/// frontmatter shapes plus project scoping, under `home`. Also covers the
/// three WU-5 regressions: an inline top-level `anchors:` naming the same
/// paths as the existing block-style `anchor-fact.md` (item 1), a duplicate
/// top-level key that changes shape (item 2), and a bare `\r` embedded
/// mid-frontmatter (item 3).
fn populate_fixture_tree(home: &Path) {
    write_fact(
        home,
        "scalar-fact.md",
        "---\nname: scalar-fact\ntype: reference\nlinks:\n  relates_to: other-fact\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "list-fact.md",
        "---\nname: list-fact\ntype: reference\nlinks:\n  relates_to: [alpha, beta, \"gamma\"]\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "nested-fact.md",
        "---\nname: nested-fact\ntype: reference\nlinks:\n  relates_to:\n    - item-one\n    - item-two\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "anchor-fact.md",
        "---\nname: anchor-fact\ntype: reference\nanchors:\n  - src/index.ts\n  - src/other.ts\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "fact-v1.md",
        "---\nname: fact-v1\ntype: reference\nlinks:\n  supersedes: fact-v2\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "fact-v2.md",
        "---\nname: fact-v2\ntype: reference\nlinks:\n  supersedes: fact-v3\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "fact-v3.md",
        "---\nname: fact-v3\ntype: reference\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "acme/widget/proj-fact.md",
        "---\nname: proj-fact\ntype: reference\nlinks:\n  relates_to: sibling-fact\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "no-frontmatter.md",
        "Just a note with no frontmatter at all.\n",
    );
    write_fact(
        home,
        "inline-anchor-fact.md",
        "---\nname: inline-anchor-fact\ntype: reference\nanchors: [src/index.ts, src/other.ts]\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "duplicate-key-fact.md",
        "---\nname: duplicate-key-fact\ntype: reference\nlinks:\n  relates_to: a\nlinks: not-a-dict\n---\n\nBody text.\n",
    );
    write_fact(
        home,
        "cr-fact.md",
        "---\nname: cr-fact\ntype: reference\ndescription: before\rafter\n---\n\nBody text.\n",
    );
}

/// A node is uniquely keyed by its `id`; an edge has no id, so its
/// `(from, to, relation)` triple serves the same purpose. One key function
/// covering both shapes, since it is only ever applied within one
/// homogeneous array at a time.
fn sort_key(v: &Value) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        v.get("id").and_then(Value::as_str).unwrap_or(""),
        v.get("from").and_then(Value::as_str).unwrap_or(""),
        v.get("to").and_then(Value::as_str).unwrap_or(""),
        v.get("relation").and_then(Value::as_str).unwrap_or(""),
    )
}

/// The graph in `memory.db` with its `nodes`/`edges` arrays sorted by a
/// canonical key, for comparison that ignores array order.
fn canonical_graph(home: &Path) -> Value {
    let mut value = read_graph(home);
    if let Some(arr) = value.get_mut("nodes").and_then(Value::as_array_mut) {
        arr.sort_by_key(sort_key);
    }
    if let Some(arr) = value.get_mut("edges").and_then(Value::as_array_mut) {
        arr.sort_by_key(sort_key);
    }
    value
}

/// Run the python writer and the Rust writer over the same fixture memory
/// tree and assert the two graphs are equal.
///
/// Byte-for-byte equality is not attempted here and would not be
/// meaningful: python's `os.walk` and Rust's `fs::read_dir` both return
/// directory entries in unspecified, implementation-defined order, so the
/// two `nodes`/`edges` arrays can legitimately come out in different orders
/// even though they contain the same elements; `json.dump(..., indent=2)`
/// and `serde_json::to_string_pretty` also do not format identically. This
/// test instead parses both outputs and compares them with `nodes` and
/// `edges` each sorted by a canonical key, i.e. semantic equality on the
/// parsed JSON. This is a deliberate, explicitly flagged choice per the
/// WU-5 brief, not a silent weakening of the assertion.
#[test]
fn rust_writer_matches_the_frozen_python_golden() {
    // Arrange
    let home_rs = scratch_home("golden-rs");
    populate_fixture_tree(&home_rs);

    // Act
    run_rebuild_for(&home_rs, "scalar-fact.md");

    // Assert against the frozen python oracle rather than a live python run.
    // See tests/fixtures/golden/README.md: the python original is deleted by
    // ADR 0007 WU-14, so its output is committed instead. This keeps the
    // cross-implementation check that a ported-only suite cannot give, since
    // a ported suite passes against an empty stub.
    let mut rust_graph = canonical_graph(&home_rs);
    assert_eq!(
        rust_graph["version"], 1,
        "graph output should carry \"version\":1"
    );
    // The frozen golden predates the version field, so it is stripped before
    // the structural comparison below; the assertion above already covers it.
    rust_graph
        .as_object_mut()
        .expect("graph should be a JSON object")
        .remove("version");

    // The frozen golden also predates possible_relates_to edges: the python
    // original never computed a similarity signal, so the fixture tree's
    // many same-type, same-scope, same-placeholder-body facts legitimately
    // pick up possible_relates_to edges among themselves that the golden
    // cannot know about. Stripped here for the same reason "version" is
    // stripped above, not a weakening of the structural comparison for
    // every relation the golden does cover.
    if let Some(edges) = rust_graph.get_mut("edges").and_then(Value::as_array_mut) {
        edges.retain(|e| e["relation"] != "possible_relates_to");
    }

    let golden: Value = serde_json::from_str(include_str!(
        "fixtures/golden/rebuild-memory-graph.scalar-fact.json"
    ))
    .expect("golden fixture should be valid JSON");
    assert_eq!(
        golden, rust_graph,
        "rust memory.db drifted from the frozen python output"
    );

    let _ = fs::remove_dir_all(&home_rs);
}

/// `playbook memory rebuild` rebuilds with NO payload at all.
///
/// This is the `--graph-only` path of `/playbook:learn`. It used to
/// run `python3 the retired shell original < /dev/null`, because that
/// script treated empty stdin as "rebuild everything". The Rust port dropped
/// that branch deliberately (`should_skip` in
/// `src/hooks/rebuild_memory_graph.rs`), reasoning it was unexercised by the
/// hook's own test suite. It was exercised, by a slash command rather than a
/// test, so deleting the python would have broken it silently.
///
/// The assertion that matters is the contrast: the hook SKIPS the same tree
/// when handed a payload naming a file outside the memory dir, while the
/// subcommand rebuilds regardless. A test that only checked the subcommand
/// would still pass if the forced path quietly regained a skip check.
#[test]
fn memory_rebuild_subcommand_rebuilds_with_no_payload_where_the_hook_skips() {
    // Arrange
    let home = scratch_home("memory-rebuild");
    populate_fixture_tree(&home);
    let graph = graph_path(&home);

    // Act: the hook, told about a file outside the memory dir, must not rebuild
    let hook = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .args(["hook", "rebuild-memory-graph"])
        .env("HOME", &home)
        .env(
            "HOOK_INPUT",
            r#"{"tool_input":{"file_path":"/tmp/elsewhere.md"}}"#,
        )
        .output()
        .expect("binary should run");

    // Assert
    assert!(
        hook.status.success(),
        "the hook should exit 0 when skipping"
    );
    assert!(
        !graph.exists(),
        "the hook must not rebuild for a path outside the memory dir"
    );

    // Act: the subcommand, with no payload whatsoever
    let forced = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .args(["memory", "rebuild"])
        .env("HOME", &home)
        .output()
        .expect("binary should run");

    // Assert
    assert!(forced.status.success(), "memory rebuild should exit 0");
    assert!(
        graph.exists(),
        "memory rebuild must rebuild with no payload, which is what --graph-only needs"
    );
    let value = canonical_graph(&home);
    assert!(
        !value["nodes"].as_array().unwrap().is_empty(),
        "the rebuilt graph should carry the fixture tree's facts, not be empty"
    );

    let _ = fs::remove_dir_all(&home);
}

/// The real interleaving race (two subprocesses' rebuilds overlapping) is
/// too narrow a window to hit reliably in a test: subprocess spawn overhead
/// dwarfs the tiny fixture tree's read-build-write time, so the two
/// processes rarely truly overlap. This test instead exercises the lock
/// wiring deterministically: pre-create `memory.graph.json.lock` as a directory
/// (simulating another session mid-rebuild), then run a rebuild against it.
/// The lock is advisory and fails open after its retry budget (matching
/// `common::atomic`'s documented contract: a hook must never hang on
/// contention), so this must still succeed and produce a correct graph, not
/// hang or error out just because the lock directory was already there.
#[test]
fn rebuild_succeeds_when_the_lock_directory_is_already_held() {
    // Arrange
    let home = scratch_home("lock-contended");
    write_fact(
        &home,
        "fact-under-lock.md",
        "---\nname: fact-under-lock\ntype: reference\n---\n\nBody text.\n",
    );
    let lock_dir = home
        .join(".config")
        .join("playbook")
        .join("memory")
        .join("memory.graph.json.lock");
    fs::create_dir(&lock_dir).expect("pre-creating the lock dir should succeed");

    // Act
    let out = run_rebuild_for(&home, "fact-under-lock.md");

    // Assert
    assert!(
        out.status.success(),
        "rebuild must fail open, not hang or error, when the lock is already held: {out:?}"
    );
    let graph = read_graph(&home);
    assert!(
        has_node(&graph, "global/fact-under-lock"),
        "the graph must still be correct after a fail-open rebuild"
    );
    // The pre-existing lock dir was never this run's to remove (it did not
    // create it), so it must still be there: removing a lock directory you
    // did not acquire would destroy another process's in-progress lock.
    assert!(
        lock_dir.exists(),
        "a rebuild that did not acquire the lock must not remove it"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Two sessions saving different facts near the same moment each trigger a
/// full rebuild. Without a lock around the read-build-write cycle, the
/// rebuild that started from a staler view of the tree can still finish
/// (and rename) after the fresher one, silently dropping whatever the
/// fresher rebuild had just seen. Both concurrent rebuilds must survive in
/// the final graph, since real concurrent processes (not threads inside one
/// process) are what an mkdir-based lock actually has to serialize.
#[test]
fn concurrent_rebuilds_from_two_sessions_both_survive() {
    // Arrange
    let home = scratch_home("concurrent-rebuild");
    write_fact(
        &home,
        "fact-a.md",
        "---\nname: fact-a\ntype: reference\n---\n\nBody text.\n",
    );
    write_fact(
        &home,
        "fact-b.md",
        "---\nname: fact-b\ntype: reference\n---\n\nBody text.\n",
    );

    // Act: two real subprocesses, each rebuilding for a different fact,
    // launched from separate threads so they actually overlap.
    let home_a = home.clone();
    let home_b = home.clone();
    let a = std::thread::spawn(move || run_rebuild_for(&home_a, "fact-a.md"));
    let b = std::thread::spawn(move || run_rebuild_for(&home_b, "fact-b.md"));
    let out_a = a.join().expect("thread a should not panic");
    let out_b = b.join().expect("thread b should not panic");

    // Assert
    assert!(out_a.status.success(), "rebuild for fact-a should exit 0");
    assert!(out_b.status.success(), "rebuild for fact-b should exit 0");
    let graph = read_graph(&home);
    assert!(
        has_node(&graph, "global/fact-a"),
        "fact-a must survive a concurrent rebuild, not be silently dropped"
    );
    assert!(
        has_node(&graph, "global/fact-b"),
        "fact-b must survive a concurrent rebuild, not be silently dropped"
    );

    let _ = fs::remove_dir_all(&home);
}

// --- possible_relates_to similarity edges -----------------------------------

/// A pair sharing an anchor parent directory and matching type+scope gets a
/// `possible_relates_to` edge naming exactly those two signals. The two
/// bodies use entirely disjoint word sets, so the Jaccard signal cannot also
/// fire, proving the edge comes from exactly 2 signals, not 3.
#[test]
fn pair_sharing_anchor_dir_and_type_scope_gets_the_edge_with_both_signals_named() {
    // Arrange
    let home = scratch_home("similarity-anchor-and-type-scope");
    write_fact(
        &home,
        "shared-a.md",
        "---\nname: shared-a\ntype: reference\nanchors: [src/foo/a.ts]\n---\n\nApples oranges bananas grapes melons.\n",
    );
    write_fact(
        &home,
        "shared-b.md",
        "---\nname: shared-b\ntype: reference\nanchors: [src/foo/b.ts]\n---\n\nRockets satellites telescopes asteroids comets.\n",
    );

    // Act
    run_rebuild_for(&home, "shared-a.md");

    // Assert
    let graph = read_graph(&home);
    let signals = possible_relates_to_signals(&graph, "global/shared-a", "global/shared-b")
        .expect("a possible_relates_to edge should exist between shared-a and shared-b");
    assert_eq!(
        signals,
        &vec![json!("anchor_dir"), json!("type_scope")],
        "the edge should name exactly the anchor-directory and type/scope signals"
    );

    let _ = fs::remove_dir_all(&home);
}

/// A pair with at most 1 matching signal (here: matching type+scope alone)
/// gets no `possible_relates_to` edge. Both facts share a type and scope,
/// but neither anchor nor body text overlap, so the pair sits one signal
/// short of the 2-of-3 threshold.
#[test]
fn dissimilar_pair_with_only_one_signal_gets_no_possible_relates_to_edge() {
    // Arrange
    let home = scratch_home("similarity-one-signal-only");
    write_fact(
        &home,
        "solo-a.md",
        "---\nname: solo-a\ntype: reference\n---\n\nApples oranges bananas grapes.\n",
    );
    write_fact(
        &home,
        "solo-b.md",
        "---\nname: solo-b\ntype: reference\n---\n\nRockets satellites telescopes asteroids.\n",
    );

    // Act
    run_rebuild_for(&home, "solo-a.md");

    // Assert
    let graph = read_graph(&home);
    assert!(
        possible_relates_to_signals(&graph, "global/solo-a", "global/solo-b").is_none(),
        "a pair with only the type/scope signal matching should get no possible_relates_to edge"
    );

    let _ = fs::remove_dir_all(&home);
}

/// A fact is never compared against itself. A lone fact, anchored and with
/// a non-empty body, would trivially match all 3 signals against itself if
/// the pairwise loop ever let `i == j`: same type and scope, its own
/// anchors always share their own directory, and its body is identical to
/// itself (Jaccard 1.0). No such self edge may appear.
#[test]
fn a_fact_is_never_compared_against_itself() {
    // Arrange
    let home = scratch_home("similarity-no-self-comparison");
    write_fact(
        &home,
        "solo-fact.md",
        "---\nname: solo-fact\ntype: reference\nanchors: [src/solo/x.ts]\n---\n\nSome meaningful text here for the solo fact only.\n",
    );

    // Act
    run_rebuild_for(&home, "solo-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert!(
        edges(&graph).iter().all(|e| e["from"] != e["to"]),
        "no edge should ever have from == to"
    );
    assert!(
        !edges(&graph)
            .iter()
            .any(|e| e["relation"] == "possible_relates_to"),
        "a single fact has no partner to compare against, so no possible_relates_to edge should exist"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Two facts each anchoring a different bare top-level file (no parent
/// directory) must NOT count as sharing an anchor directory. Both anchors
/// parent to the empty string, but that is not a real shared directory: the
/// anchor_dir signal must exclude the empty-string bucket, or any two
/// top-level-anchored facts would falsely match on this signal alone.
#[test]
fn top_level_anchors_with_no_shared_directory_do_not_match_the_anchor_dir_signal() {
    // Arrange
    let home = scratch_home("similarity-top-level-anchors");
    write_fact(
        &home,
        "readme-fact.md",
        "---\nname: readme-fact\ntype: reference\nanchors: [README.md]\n---\n\nDescribes the project setup and layout.\n",
    );
    write_fact(
        &home,
        "package-fact.md",
        "---\nname: package-fact\ntype: reference\nanchors: [package.json]\n---\n\nLists dependencies and build scripts.\n",
    );

    // Act
    run_rebuild_for(&home, "readme-fact.md");
    run_rebuild_for(&home, "package-fact.md");

    // Assert
    let graph = read_graph(&home);
    assert!(
        !edges(&graph)
            .iter()
            .any(|e| e["relation"] == "possible_relates_to"),
        "two facts anchoring unrelated top-level files must not match on anchor_dir alone"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Threshold boundary: two bodies whose Jaccard ratio lands exactly on
/// `SIMILARITY_JACCARD_THRESHOLD` (0.35) still count as a hit, confirming
/// the comparison is `>=`, not `>`.
///
/// Word-set arithmetic (checkable by hand): 7 words are shared by both
/// bodies (alpha..eta), body A adds 3 more words found nowhere else
/// (theta, iota, kappa: |A| = 10), body B adds 10 more words found nowhere
/// else (lambda..upsilon: |B| = 17). Union = 7 shared + 3 A-only + 10 B-only
/// = 20 distinct words. Intersection = the 7 shared words. 7 / 20 = 0.35,
/// exactly `SIMILARITY_JACCARD_THRESHOLD`.
///
/// The two facts also share the same type and scope (a second, deliberate
/// signal match), since a pair needs 2 of 3 signals to get an edge at all:
/// this isolates whether the Jaccard signal itself counts at the exact
/// boundary, without also needing an anchor-directory match (neither fact
/// has anchors).
#[test]
fn threshold_boundary_jaccard_score_of_exactly_the_constant_counts_as_a_hit() {
    // Arrange
    let home = scratch_home("similarity-threshold-boundary");
    write_fact(
        &home,
        "boundary-a.md",
        "---\nname: boundary-a\ntype: reference\n---\n\nalpha beta gamma delta epsilon zeta eta theta iota kappa\n",
    );
    write_fact(
        &home,
        "boundary-b.md",
        "---\nname: boundary-b\ntype: reference\n---\n\nalpha beta gamma delta epsilon zeta eta lambda mu nu xi omicron pi rho sigma tau upsilon\n",
    );

    // Act
    run_rebuild_for(&home, "boundary-a.md");

    // Assert
    let graph = read_graph(&home);
    let signals = possible_relates_to_signals(&graph, "global/boundary-a", "global/boundary-b")
        .expect("a Jaccard ratio exactly at the threshold should count as a hit");
    assert_eq!(
        signals,
        &vec![json!("type_scope"), json!("body_overlap")],
        "the edge should name exactly the type/scope and body-overlap signals"
    );

    let _ = fs::remove_dir_all(&home);
}

/// A pair where both bodies are empty must not count the Jaccard signal: an
/// empty union can never divide-by-zero-panic and can never default to a
/// match. The other two signals (shared anchor directory, matching
/// type+scope) both hit on their own, so the edge still appears, but its
/// signals list proves the empty-body Jaccard check contributed nothing.
#[test]
fn empty_body_pair_does_not_count_as_a_jaccard_hit_and_does_not_crash() {
    // Arrange
    let home = scratch_home("similarity-empty-body");
    write_fact(
        &home,
        "empty-a.md",
        "---\nname: empty-a\ntype: reference\nanchors:\n  - src/empty/a.ts\n---\n",
    );
    write_fact(
        &home,
        "empty-b.md",
        "---\nname: empty-b\ntype: reference\nanchors:\n  - src/empty/b.ts\n---\n",
    );

    // Act
    let output = run_rebuild_for(&home, "empty-a.md");

    // Assert
    assert_eq!(
        output.status.code(),
        Some(0),
        "an empty-body pair must not crash the hook"
    );
    let graph = read_graph(&home);
    let signals = possible_relates_to_signals(&graph, "global/empty-a", "global/empty-b")
        .expect("the anchor-directory and type/scope signals alone should still produce an edge");
    assert_eq!(
        signals,
        &vec![json!("anchor_dir"), json!("type_scope")],
        "an empty-body pair must not count the body-overlap signal"
    );

    let _ = fs::remove_dir_all(&home);
}

/// `/playbook:learn --stage` writes unapproved candidates under
/// `<owner>/<repo>/staging/`; they must not become nodes, while sibling
/// facts still do.
#[test]
fn memory_rebuild_skips_the_staging_directory_and_still_indexes_siblings() {
    // Arrange
    let home = scratch_home("skip-staging");
    let fact = "---\nname: fact\ntype: reference\n---\n\nBody.\n";
    write_fact(&home, "acme/api/live-fact.md", fact);
    write_fact(&home, "acme/api/staging/staged-fact.md", fact);

    // Act
    let out = run_playbook(&home, &["memory", "rebuild"], "");

    // Assert
    assert!(out.status.success(), "rebuild failed: {out:?}");
    let graph = read_graph(&home);
    assert!(has_node(&graph, "acme/api/live-fact"));
    assert!(
        !nodes(&graph)
            .iter()
            .any(|n| n["id"].as_str().is_some_and(|id| id.contains("staging"))),
        "a staged fact must not become a node: {graph}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn two_rebuilds_of_the_same_store_write_the_same_bytes() {
    // Several link relations on one fact: a HashMap walk used to change the
    // edge order from run to run.
    let home = scratch_home("deterministic");
    write_fact(
        &home,
        "target-a.md",
        "---\nname: a\ntype: reference\n---\nBody a\n",
    );
    write_fact(
        &home,
        "target-b.md",
        "---\nname: b\ntype: reference\n---\nBody b\n",
    );
    write_fact(
        &home,
        "source.md",
        "---\nname: source\ntype: reference\nlinks:\n  relates_to: target-a\n  depends_on: target-b\n  supersedes: target-a\n  contradicts: target-b\n---\nBody\n",
    );
    assert!(run_rebuild_for(&home, "source.md").status.success());
    let first = read_graph(&home);
    for _ in 0..5 {
        assert!(run_rebuild_for(&home, "source.md").status.success());
        assert_eq!(read_graph(&home), first);
    }
}

#[test]
fn a_large_store_takes_the_threaded_path_and_still_finds_the_similar_pair() {
    // 300 facts is past the point where the pair loop uses threads.
    let home = scratch_home("threaded");
    for i in 0..300 {
        write_fact(
            &home,
            &format!("fact-{i:03}.md"),
            &format!(
                "---\nname: fact-{i:03}\ntype: reference\n---\nunique{i} words{i} only{i} here{i} for{i} this{i} fact{i} body{i}\n"
            ),
        );
    }
    let shared = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
    write_fact(
        &home,
        "twin-a.md",
        &format!("---\nname: twin-a\ntype: reference\n---\n{shared}\n"),
    );
    write_fact(
        &home,
        "twin-b.md",
        &format!("---\nname: twin-b\ntype: reference\n---\n{shared}\n"),
    );
    assert!(run_rebuild_for(&home, "twin-a.md").status.success());
    let first = read_graph(&home);
    let graph = read_graph(&home);
    let signals = possible_relates_to_signals(&graph, "global/twin-a", "global/twin-b")
        .expect("the twins share type_scope and body_overlap");
    assert_eq!(signals, &vec![json!("type_scope"), json!("body_overlap")]);
    assert!(run_rebuild_for(&home, "twin-a.md").status.success());
    assert_eq!(read_graph(&home), first);
}

// --- memory.db: the one store of the graph -----------------------------

fn memory_db(home: &Path) -> PathBuf {
    graph_path(home)
}

const PINNED_FACT: &str = "---\nname: a-fact\ndescription: d\ntype: feedback\npinned: true\nanchors:\n  - code:~/.zshrc\n---\nbody\n";

fn export(home: &Path) -> Output {
    run_playbook(home, &["memory", "export", "--json"], "")
}

#[test]
fn a_rebuild_writes_the_graph_into_memory_db_and_no_json_file() {
    let home = scratch_home("memdb-same");
    write_fact(&home, "global/a-fact.md", PINNED_FACT);

    let out = run_rebuild_for(&home, "global/a-fact.md");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(memory_db(&home).is_file());
    assert!(
        !legacy_json_path(&home).exists(),
        "memory.graph.json is not written any more"
    );
    let exported = export(&home);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let from_cli: Value = serde_json::from_slice(&exported.stdout).expect("export is JSON");
    assert_eq!(from_cli, read_graph(&home));
    assert!(has_node(&from_cli, "global/a-fact"));
    assert_eq!(from_cli["version"], 1);
}

#[test]
fn export_builds_the_database_when_it_is_missing() {
    let home = scratch_home("memdb-missing");
    write_fact(&home, "global/a-fact.md", PINNED_FACT);

    let exported = run_playbook(&home, &["memory", "export", "--json"], "");

    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    assert!(memory_db(&home).is_file());
    let doc: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert!(has_node(&doc, "global/a-fact"));
}

#[test]
fn a_second_rebuild_replaces_the_rows_and_does_not_add_to_them() {
    let home = scratch_home("memdb-twice");
    write_fact(&home, "global/a-fact.md", PINNED_FACT);
    run_rebuild_for(&home, "global/a-fact.md");
    fs::remove_file(home.join(".config/playbook/memory/global/a-fact.md")).unwrap();
    write_fact(
        &home,
        "global/b-fact.md",
        PINNED_FACT.replace("a-fact", "b-fact").as_str(),
    );

    run_rebuild_for(&home, "global/b-fact.md");

    let exported = run_playbook(&home, &["memory", "export", "--json"], "");
    let doc: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert!(has_node(&doc, "global/b-fact"));
    assert!(
        !has_node(&doc, "global/a-fact"),
        "the removed fact is gone from the db"
    );
}

#[test]
fn two_files_naming_one_node_leave_one_node_in_both_stores() {
    let home = scratch_home("memdb-collide");
    write_fact(&home, "x.md", PINNED_FACT.replace("a-fact", "x").as_str());
    write_fact(
        &home,
        "global/x.md",
        PINNED_FACT.replace("a-fact", "x").as_str(),
    );

    let out = run_rebuild_for(&home, "global/x.md");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = read_graph(&home);
    let exported = run_playbook(&home, &["memory", "export", "--json"], "");
    let doc: Value = serde_json::from_slice(&exported.stdout).unwrap();
    let count = |g: &Value| nodes(g).iter().filter(|n| n["id"] == "global/x").count();
    assert_eq!(count(&json), 1, "the file holds the node once");
    assert_eq!(count(&doc), 1, "the database holds the node once");
    assert_eq!(nodes(&json).len(), nodes(&doc).len());
    assert_eq!(edges(&json).len(), edges(&doc).len());
}

#[test]
fn a_top_level_fact_is_a_global_node_in_the_database() {
    let home = scratch_home("memdb-top-level");
    write_fact(
        &home,
        "top.md",
        PINNED_FACT.replace("a-fact", "top").as_str(),
    );

    run_rebuild_for(&home, "top.md");

    let exported = run_playbook(&home, &["memory", "export", "--json"], "");
    let doc: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert!(has_node(&doc, "global/top"));
    assert!(has_node(&read_graph(&home), "global/top"));
}
