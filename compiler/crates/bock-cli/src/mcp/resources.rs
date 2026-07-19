//! The MCP resources surface: the context pack, the spec, and the stdlib
//! reference, served as readable documents.
//!
//! # No content is authored here
//!
//! Every resource is a *view* onto an artifact that already exists and
//! already has an owner: `context-pack/BOCK-CONTEXT-PACK.md`,
//! `spec/bock-spec.md`, and `docs/src/reference/stdlib/core-*.md`. This
//! module contributes navigation — section splitting, URIs, and descriptions
//! — never reference prose. A fact that is wrong in a resource is wrong in
//! its source; fix it there.
//!
//! # How the content ships
//!
//! `bock` is published to crates.io, and `cargo publish` packages only files
//! *inside* the crate directory — so `include_str!("../../../../spec/…")`
//! builds locally and yields a broken published crate. Reading from disk at
//! runtime is equally wrong: `cargo install bock` users have no checkout.
//! So the sources are mirrored into `compiler/crates/bock-cli/assets/` by
//! `tools/scripts/sync-vocab.sh` (the same pattern the VS Code extension
//! already uses for its spec asset) and pulled in with [`include_str!`].
//! The content is therefore version-locked to the binary by construction,
//! a renamed or deleted source is a *build* error, and content drift is
//! caught by the "vocab + spec assets in sync" CI job.
//!
//! # Tiers and URIs
//!
//! | Tier | URI | Reach for it when |
//! |---|---|---|
//! | Context pack | `bock://pack/<n>`, `bock://pack/all` | writing Bock; conceptual, task-shaped |
//! | Spec | `bock://spec/<n>` | settling what is *legal*; normative |
//! | Stdlib | `bock://stdlib/<module>` | looking up an API signature |
//!
//! plus `bock://index`, the orientation document listing all three tiers.
//!
//! Spec URIs are keyed on the plain section number precisely so that the
//! `spec_refs` a `bock_explain` call returns (`"§10"`, `"§10.3"`) are
//! mechanically resolvable: see [`spec_ref_resources`], which is what turns
//! a diagnostic code into a spec section an agent can actually read.
//!
//! # Silent degradation is the real risk
//!
//! `include_str!` and the CI drift job together cover renames and content
//! drift. What neither covers: the heading format shifting so that
//! [`numbered_sections`] matches nothing and the server cheerfully lists
//! three resources instead of twenty-three, with no error anywhere. The
//! integration tests in `tests/mcp_server.rs` pin count floors and known
//! anchors for exactly that failure mode.

use serde_json::{json, Value};

/// The MIME type every resource served here carries.
const MARKDOWN: &str = "text/markdown";

/// The AI context pack — the primary, conceptual tier.
const PACK_MD: &str = include_str!("../../assets/context-pack/BOCK-CONTEXT-PACK.md");

/// The language specification — the normative tier.
const SPEC_MD: &str = include_str!("../../assets/spec/bock-spec.md");

/// The stdlib reference pages, as `(module name, page markdown)`.
///
/// Listed explicitly rather than globbed: `include_str!` needs literal
/// paths, and the explicitness means a page deleted from
/// `docs/src/reference/stdlib/` (and thus from the synced assets) fails the
/// build instead of silently vanishing from the served surface.
const STDLIB_PAGES: &[(&str, &str)] = &[
    (
        "core.collections",
        include_str!("../../assets/stdlib/core-collections.md"),
    ),
    (
        "core.compare",
        include_str!("../../assets/stdlib/core-compare.md"),
    ),
    (
        "core.convert",
        include_str!("../../assets/stdlib/core-convert.md"),
    ),
    (
        "core.effect",
        include_str!("../../assets/stdlib/core-effect.md"),
    ),
    (
        "core.error",
        include_str!("../../assets/stdlib/core-error.md"),
    ),
    (
        "core.iter",
        include_str!("../../assets/stdlib/core-iter.md"),
    ),
    (
        "core.option",
        include_str!("../../assets/stdlib/core-option.md"),
    ),
    (
        "core.result",
        include_str!("../../assets/stdlib/core-result.md"),
    ),
    (
        "core.string",
        include_str!("../../assets/stdlib/core-string.md"),
    ),
    (
        "core.test",
        include_str!("../../assets/stdlib/core-test.md"),
    ),
    (
        "core.time",
        include_str!("../../assets/stdlib/core-time.md"),
    ),
];

/// Curated one-line "reach for this when" descriptions for the pack's
/// sections, keyed on section number.
///
/// Descriptions are navigation metadata, not reference content: they say
/// *when to open* a section, which its title cannot. A section without an
/// entry falls back to its own opening prose (see [`fallback_description`]),
/// so adding a section to the pack never breaks the surface.
const PACK_DESCRIPTIONS: &[(u32, &str)] = &[
    (1, "The mental model: what feature-declarative, target-agnostic actually means, and the five v1 targets. Read this before writing any Bock."),
    (2, "The commands you will actually run — check, run, test, build, fmt, explain — with the flags that matter and what each one's output means."),
    (3, "The language primer: modules and imports, declarations, types, control flow, effects, and context, in the density an agent needs to start writing correct code."),
    (4, "The v1 boundary — surface that does NOT exist. The single most common failure is inventing `std.*` APIs; this section is the antidote."),
    (5, "The diagnostic codes the compiler emits, grouped by subsystem, with what triggers each. Pair with the bock_explain tool for a single code."),
    (6, "Five complete worked examples that all pass `bock check` — the fastest way to see idiomatic multi-feature Bock rather than isolated snippets."),
    (7, "Pitfalls from writing Bock like another language: required parens, separator rules, and the other habits that produce E2000-class errors."),
    (8, "Known divergences: true statements about the current implementation that contradict the spec. Check here before 'fixing' code the compiler rejects."),
];

/// Curated "reach for this when" descriptions for the spec's sections.
/// Same fallback rule as [`PACK_DESCRIPTIONS`].
const SPEC_DESCRIPTIONS: &[(u32, &str)] = &[
    (1, "What Bock is, its design goals, and where it sits between a natural-language prompt and target-language code."),
    (2, "One complete annotated module showing the whole syntax at a glance — the fastest orientation in the spec."),
    (3, "Lexical structure: encoding, tokens, keywords (including reserved-for-v1.x ones), literals, comments, operators."),
    (4, "The type system: structural typing, inference, primitives, generics, traits, optionals and results. Normative for E4xxx diagnostics."),
    (5, "The lightweight ownership rules that let one source compile to both GC and manual-memory targets. Normative for ownership diagnostics."),
    (6, "Declaration forms: functions, records, enums, traits, impls, constants, and their modifiers and visibility."),
    (7, "Expressions: expression-valued control flow, operator precedence, lambdas, calls, field and index access."),
    (8, "Statements and control flow: let bindings, assignment, if/while/for/guard, return — and the parenthesization rules."),
    (9, "Pattern matching: the pattern kinds, match-arm syntax, guards, and exhaustiveness requirements."),
    (10, "The effect system: effect declarations, handlers, propagation, and purity. Normative for effect diagnostics."),
    (11, "The context system: what @context metadata means, where it is required, and how the checker validates it."),
    (12, "The module system: file-to-module-path mapping, the braced `use` form, visibility, and cross-file resolution."),
    (13, "Concurrency: the concurrent execution forms and how each target realizes them."),
    (14, "Interop and FFI, including `native` blocks (v1 reserves the keyword; the surface lands in v1.x)."),
    (15, "The complete annotation taxonomy, organized by which compiler subsystem consumes each `@` annotation."),
    (16, "AIR, the four-layer annotated IR the front end produces and the transpiler consumes."),
    (17, "The transpilation pipeline: stages from source to target code, determinism rules, and where AI transpilation enters."),
    (18, "The standard library architecture: the two `core`/`std` tiers, the v1 `core.*` surface, and what is specced but unshipped."),
    (19, "The package manager: bock.project, bock.lock, dependency resolution, and target-aware dependencies."),
    (20, "Tooling: the normative CLI capability surface, the LSP, the formatter, and the servers (including this MCP server)."),
    (21, "The complete EBNF grammar — the authoritative answer to whether a given piece of syntax is legal."),
    (22, "Target profiles: the per-target capability profile the transpiler consults for each of the five v1 targets."),
    (23, "Appendices: the bock.project reference, reserved words, and other lookup tables."),
];

/// One parsed `## <n>. <title>` section of a markdown document.
struct Section {
    /// The section number from the heading.
    number: u32,
    /// The heading text after `<n>. `.
    title: String,
    /// The full section text, heading line included.
    body: String,
}

/// Split a markdown document into its top-level numbered sections.
///
/// Matches only `## <digits>. <title>` at the start of a line and outside a
/// fenced code block, so a document's table of contents (`## Table of
/// Contents`) and any `#`-prefixed lines inside fences are skipped. A
/// section runs to the next such heading — or to any other `## ` heading,
/// so trailing unnumbered matter is not absorbed.
fn numbered_sections(markdown: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        let heading = if in_fence {
            None
        } else {
            parse_numbered_heading(line)
        };
        match heading {
            Some((number, title)) => sections.push(Section {
                number,
                title,
                body: line.to_string(),
            }),
            None => {
                if !in_fence && line.starts_with("## ") {
                    // An unnumbered top-level heading closes the previous
                    // section rather than being folded into it.
                    if let Some(last) = sections.last_mut() {
                        if !last.body.is_empty() {
                            sections.push(Section {
                                number: 0,
                                title: String::new(),
                                body: String::new(),
                            });
                        }
                    }
                    continue;
                }
                if let Some(last) = sections.last_mut() {
                    last.body.push('\n');
                    last.body.push_str(line);
                }
            }
        }
    }
    sections.retain(|s| s.number != 0);
    sections
}

/// Parse `## <n>. <title>` into its number and title, if the line is one.
fn parse_numbered_heading(line: &str) -> Option<(u32, String)> {
    let rest = line.strip_prefix("## ")?;
    let (digits, title) = rest.split_once(". ")?;
    let number: u32 = digits.parse().ok()?;
    Some((number, title.trim().to_string()))
}

/// Look up a curated description, falling back to the document's own words.
fn describe(curated: &[(u32, &str)], section: &Section) -> String {
    curated
        .iter()
        .find(|(n, _)| *n == section.number)
        .map(|(_, d)| (*d).to_string())
        .unwrap_or_else(|| fallback_description(&section.body))
}

/// Derive a description from a document's first paragraph of prose.
///
/// Used when no curated description exists (a newly added section, or a
/// stdlib page). Skips headings, fences, and list markers so the result is
/// a sentence rather than a fragment of a code block.
fn fallback_description(body: &str) -> String {
    let mut paragraph = String::new();
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with('|') {
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    if paragraph.is_empty() {
        return "No summary available; read the resource.".to_string();
    }
    truncate_on_word(&paragraph, 260)
}

/// Truncate to at most `limit` bytes on a word boundary, adding an ellipsis.
fn truncate_on_word(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    // Back off to the nearest char boundary at or below the limit, so the
    // slice below cannot split a multi-byte character.
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = text[..cut].trim_end();
    let boundary = head.rfind(' ').unwrap_or(head.len());
    format!("{}…", head[..boundary].trim_end())
}

/// One entry of the served surface: its URI, metadata, and content.
struct Resource {
    uri: String,
    name: String,
    title: String,
    description: String,
    text: String,
}

impl Resource {
    /// The `resources/list` entry shape (metadata only — no content).
    fn listing(&self) -> Value {
        json!({
            "uri": self.uri,
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "mimeType": MARKDOWN,
        })
    }
}

/// Build the full resource set, in the order `resources/list` reports it:
/// the index, then the pack, then the spec, then the stdlib.
fn all_resources() -> Vec<Resource> {
    let pack = numbered_sections(PACK_MD);
    let spec = numbered_sections(SPEC_MD);

    let mut resources = Vec::with_capacity(pack.len() + spec.len() + STDLIB_PAGES.len() + 2);
    resources.push(index_resource(&pack, &spec));

    for section in &pack {
        resources.push(Resource {
            uri: format!("bock://pack/{}", section.number),
            name: format!("pack-{}", section.number),
            title: format!("Context pack §{}. {}", section.number, section.title),
            description: describe(PACK_DESCRIPTIONS, section),
            text: section.body.clone(),
        });
    }
    resources.push(Resource {
        uri: "bock://pack/all".to_string(),
        name: "pack-all".to_string(),
        title: "Context pack (complete, ~12k tokens)".to_string(),
        description: "OPT-IN, LARGE: the entire context pack in one read, \
             roughly 12k tokens. Do not open this to answer a specific \
             question — read the individual bock://pack/<n> section instead. \
             Reach for it only when you deliberately want the whole pack \
             resident, e.g. seeding a long Bock-authoring session."
            .to_string(),
        text: PACK_MD.to_string(),
    });

    for section in &spec {
        resources.push(Resource {
            uri: format!("bock://spec/{}", section.number),
            name: format!("spec-{}", section.number),
            title: format!("Spec §{}. {}", section.number, section.title),
            description: describe(SPEC_DESCRIPTIONS, section),
            text: section.body.clone(),
        });
    }

    for (module, page) in STDLIB_PAGES {
        resources.push(Resource {
            uri: format!("bock://stdlib/{module}"),
            name: format!("stdlib-{module}"),
            title: format!("Stdlib reference: {module}"),
            description: fallback_description(page),
            text: (*page).to_string(),
        });
    }

    resources
}

/// Build the index: the one resource that orients an agent across the tiers.
///
/// Generated from the parsed sections rather than hand-maintained, so it
/// cannot list a resource that does not exist (or miss one that does).
fn index_resource(pack: &[Section], spec: &[Section]) -> Resource {
    let mut text = String::from(
        "# Bock MCP resources — index\n\
         \n\
         Three tiers, three jobs. Pick by the question you are answering.\n\
         \n\
         | Tier | Use it for | URIs |\n\
         |---|---|---|\n\
         | **Context pack** | *How do I write this?* Conceptual, task-shaped, \
         opinionated. Start here. | `bock://pack/<n>` |\n\
         | **Specification** | *Is this legal / what is the exact rule?* \
         Normative and absolute; it wins over the pack. | `bock://spec/<n>` |\n\
         | **Stdlib reference** | *What is this function's signature?* \
         API reference for the v1 `core.*` modules. | `bock://stdlib/<module>` |\n\
         \n\
         Diagnostic codes bridge into the spec: call the `bock_explain` tool \
         with a code (e.g. `E6005`) and it returns `spec_resources`, each \
         carrying the `bock://spec/<n>` URI for the section that governs it.\n\
         \n\
         ## Context pack sections\n\n",
    );
    for section in pack {
        text.push_str(&format!(
            "- `bock://pack/{}` — {}\n",
            section.number, section.title
        ));
    }
    text.push_str(
        "- `bock://pack/all` — the whole pack in one read (~12k tokens; \
         opt-in, not a first click)\n\n## Specification sections\n\n",
    );
    for section in spec {
        text.push_str(&format!(
            "- `bock://spec/{}` — {}\n",
            section.number, section.title
        ));
    }
    text.push_str("\n## Stdlib modules\n\n");
    for (module, _) in STDLIB_PAGES {
        text.push_str(&format!("- `bock://stdlib/{module}`\n"));
    }

    Resource {
        uri: "bock://index".to_string(),
        name: "index".to_string(),
        title: "Index — which Bock resource to reach for".to_string(),
        description: "START HERE. Orients you across the three tiers \
             (context pack = conceptual how-to, spec = normative rules, \
             stdlib = API reference), lists every resource with its topic, \
             and explains the diagnostic-code → spec-section bridge."
            .to_string(),
        text,
    }
}

/// The `resources/list` payload.
pub fn resource_list() -> Vec<Value> {
    all_resources().iter().map(Resource::listing).collect()
}

/// The `resources/read` payload for `uri`, or `None` if no such resource.
pub fn read_resource(uri: &str) -> Option<Value> {
    let resource = all_resources().into_iter().find(|r| r.uri == uri)?;
    Some(json!({
        "contents": [ {
            "uri": resource.uri,
            "name": resource.name,
            "title": resource.title,
            "mimeType": MARKDOWN,
            "text": resource.text,
        } ],
    }))
}

/// Resolve a diagnostic's `spec_refs` into readable spec resources.
///
/// This is the explain → spec bridge. `bock_explain` returns refs like
/// `"§10"` or `"§10.3"`, which are human strings an agent cannot act on.
/// Each ref whose top-level section exists is paired with the
/// `bock://spec/<n>` URI serving that section, so the loop
/// *diagnostic code → explain → spec section → read* closes mechanically.
/// A ref that resolves to no section is dropped rather than yielding a dead
/// URI; the original `spec_refs` strings are still reported alongside.
pub fn spec_ref_resources(spec_refs: &[&str]) -> Vec<Value> {
    let sections = numbered_sections(SPEC_MD);
    let mut out = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    for reference in spec_refs {
        let Some(number) = leading_section_number(reference) else {
            continue;
        };
        let Some(section) = sections.iter().find(|s| s.number == number) else {
            continue;
        };
        // Two refs into the same section (`§10` and `§10.3`) resolve to one
        // resource; report it once, tagged with the first ref that reached it.
        if seen.contains(&number) {
            continue;
        }
        seen.push(number);
        out.push(json!({
            "ref": reference,
            "uri": format!("bock://spec/{number}"),
            "title": section.title,
        }));
    }
    out
}

/// Extract the top-level section number from a `"§10.3"`-style reference.
fn leading_section_number(reference: &str) -> Option<u32> {
    let digits: String = reference
        .trim_start_matches('§')
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_sections_ignores_toc_and_fenced_headings() {
        let doc = "# Title\n\n## Table of Contents\n\n- a\n\n## 1. First\n\
                   \nbody\n\n```\n## 2. Not a heading\n```\n\n## 2. Second\n\nmore\n";
        let sections = numbered_sections(doc);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "First");
        assert_eq!(sections[1].number, 2);
        assert!(sections[0].body.contains("## 2. Not a heading"));
        assert!(sections[1].body.contains("more"));
    }

    #[test]
    fn every_listed_resource_reads_back_non_empty() {
        for entry in resource_list() {
            let uri = entry["uri"].as_str().expect("uri");
            let read = read_resource(uri).unwrap_or_else(|| panic!("dead URI {uri}"));
            let text = read["contents"][0]["text"].as_str().expect("text");
            assert!(!text.trim().is_empty(), "{uri} read back empty");
            let description = entry["description"].as_str().expect("description");
            assert!(description.len() > 40, "{uri} has a thin description");
        }
    }

    #[test]
    fn section_counts_do_not_silently_shrink() {
        assert!(numbered_sections(SPEC_MD).len() >= 23);
        assert!(numbered_sections(PACK_MD).len() >= 8);
        assert_eq!(STDLIB_PAGES.len(), 11);
    }

    #[test]
    fn known_anchors_resolve() {
        let effects = read_resource("bock://spec/10").expect("spec 10 exists");
        assert!(effects["contents"][0]["text"]
            .as_str()
            .expect("text")
            .contains("Effect System"));
        assert!(read_resource("bock://stdlib/core.option").is_some());
        assert!(read_resource("bock://pack/all").is_some());
        assert!(read_resource("bock://index").is_some());
        assert!(read_resource("bock://spec/999").is_none());
    }

    #[test]
    fn spec_refs_resolve_to_existing_resources() {
        let resolved = spec_ref_resources(&["§10.3", "§10", "§999", "nonsense"]);
        assert_eq!(
            resolved.len(),
            1,
            "duplicate sections collapse: {resolved:?}"
        );
        assert_eq!(resolved[0]["uri"], "bock://spec/10");
        assert!(read_resource(resolved[0]["uri"].as_str().expect("uri")).is_some());
    }

    #[test]
    fn truncation_lands_on_a_word_boundary() {
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let cut = truncate_on_word(long, 20);
        assert!(cut.ends_with('…'));
        assert!(long.starts_with(cut.trim_end_matches('…')));
        assert_eq!(truncate_on_word("short", 20), "short");
    }
}
