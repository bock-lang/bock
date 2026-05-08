//! Local codegen rule cache (§17.7).
//!
//! Rules live under `<project_root>/.bock/rules/{target_id}/{rule_id}.json`.
//! They are consulted before Tier 1 AI synthesis: if a rule matches the
//! AIR node's discriminant, its template is applied deterministically,
//! saving the AI round-trip and turning a previously learned pattern
//! into a pure lookup.
//!
//! The cache is append-only from the compiler's perspective: repair
//! produces a [`crate::request::CandidateRule`] which is
//! upgraded to a [`Rule`] (with provenance, id, timestamp) and written
//! to disk. Human curation (pinning, deleting) happens through
//! `bock override` and is out of scope for this module.
//!
//! `node_kind` is the discriminant string of [`bock_air::NodeKind`]
//! (`"Match"`, `"Call"`, …). It is captured from the node the rule was
//! extracted from; matching is kind-equal for v1.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use bock_air::{AIRNode, NodeKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cache::compute_key;
use crate::request::CandidateRule;

// ─── Provenance ──────────────────────────────────────────────────────────────

/// Where a rule came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Ships with the compiler.
    Builtin,
    /// Distilled from a successful repair pass.
    Extracted,
    /// Authored by a human and committed to the project.
    Manual,
}

// ─── Rule ────────────────────────────────────────────────────────────────────

/// A single pattern-template mapping for deterministic codegen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable identifier — SHA-256 of `(target_id, node_kind, template)`.
    pub id: String,
    /// Target language id (e.g. `"js"`, `"rust"`).
    pub target_id: String,
    /// AIR node-kind discriminant the rule matches.
    pub node_kind: String,
    /// Free-form pattern description. Not used for matching in v1; kept
    /// so humans and future pattern engines can see what the AI had in
    /// mind when it extracted the rule.
    pub pattern: String,
    /// Code template with interpolation slots (format TBD per §17.7).
    pub template: String,
    /// Where the rule came from.
    pub provenance: Provenance,
    /// Whether a human has pinned this rule (required in `production`).
    pub pinned: bool,
    /// Confidence the AI assigned at extraction time.
    pub confidence: f64,
    /// Priority for conflict resolution (higher wins).
    pub priority: i32,
    /// When the rule was recorded.
    pub created: DateTime<Utc>,
}

impl Rule {
    /// Lifts a provider-returned [`CandidateRule`] into a stored rule.
    ///
    /// `node_kind` is derived from the AIR node that triggered the
    /// repair (the provider's textual pattern is free-form, so the
    /// caller supplies the actual discriminant). Provenance defaults to
    /// [`Provenance::Extracted`]; callers at `production` strictness
    /// may pin after curation.
    #[must_use]
    pub fn from_candidate(candidate: &CandidateRule, node_kind: &str, confidence: f64) -> Self {
        let id = compute_rule_id(&candidate.target_id, node_kind, &candidate.template);
        Self {
            id,
            target_id: candidate.target_id.clone(),
            node_kind: node_kind.into(),
            pattern: candidate.pattern.clone(),
            template: candidate.template.clone(),
            provenance: Provenance::Extracted,
            pinned: false,
            confidence,
            priority: candidate.priority,
            created: Utc::now(),
        }
    }
}

/// Stable content-addressed id for a rule.
///
/// Used so repeated extractions of the same `(target, kind, template)`
/// triple collapse to one file on disk rather than accumulating
/// near-duplicates.
#[must_use]
pub fn compute_rule_id(target_id: &str, node_kind: &str, template: &str) -> String {
    #[derive(Serialize)]
    struct Keyed<'a> {
        target: &'a str,
        kind: &'a str,
        template: &'a str,
    }
    let keyed = Keyed {
        target: target_id,
        kind: node_kind,
        template,
    };
    compute_key(&keyed).unwrap_or_else(|_| format!("fallback-{target_id}-{node_kind}"))
}

// ─── RuleCache ───────────────────────────────────────────────────────────────

/// Errors produced by the rule cache.
#[derive(Debug, thiserror::Error)]
pub enum RuleCacheError {
    /// Filesystem I/O failed.
    #[error("rule cache I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON parse failed reading a stored rule file.
    #[error("rule parse error in {path}: {source}")]
    Parse {
        /// Offending file path.
        path: PathBuf,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },
    /// JSON serialization failed writing a rule.
    #[error("rule serialize error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// On-disk rule cache rooted at `<project_root>/.bock/rules/`.
#[derive(Debug, Clone)]
pub struct RuleCache {
    root: PathBuf,
}

impl RuleCache {
    /// Cache rooted at `<project_root>/.bock/rules/`.
    ///
    /// The directory is not created eagerly — it is materialised on
    /// first [`insert`](Self::insert).
    #[must_use]
    pub fn new(project_root: &Path) -> Self {
        Self {
            root: project_root.join(".bock").join("rules"),
        }
    }

    /// Cache rooted at an explicit directory. Mainly for tests.
    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    /// Path to the cache root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory where rules for `target_id` are stored.
    #[must_use]
    pub fn target_dir(&self, target_id: &str) -> PathBuf {
        self.root.join(target_id)
    }

    /// Writes `rule` to disk. Creates parent directories on demand.
    ///
    /// Idempotent: two inserts of the same [`Rule::id`] end in one file.
    ///
    /// # Errors
    /// Returns [`RuleCacheError`] on I/O or serialization failure.
    pub fn insert(&self, rule: &Rule) -> Result<(), RuleCacheError> {
        let dir = self.target_dir(&rule.target_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", rule.id));
        let bytes = serde_json::to_vec_pretty(rule)?;
        fs::write(&path, bytes)?;
        Ok(())
    }

    /// Loads every rule stored for `target_id`.
    ///
    /// # Errors
    /// Returns [`RuleCacheError`] on I/O or parse failure.
    pub fn load_for_target(&self, target_id: &str) -> Result<Vec<Rule>, RuleCacheError> {
        let dir = self.target_dir(target_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            let rule: Rule =
                serde_json::from_slice(&bytes).map_err(|source| RuleCacheError::Parse {
                    path: path.clone(),
                    source,
                })?;
            out.push(rule);
        }
        Ok(out)
    }

    /// Looks up the best-matching rule for `node` under `target_id`.
    ///
    /// Matching is kind-equal against [`node_kind_name`]. Under
    /// [`Strictness::Production`](bock_types::Strictness::Production)
    /// only pinned rules are considered — per the §17.7 strictness
    /// table. Highest priority wins; ties broken by pinned, then
    /// most-recently-created.
    ///
    /// # Errors
    /// Returns [`RuleCacheError`] on I/O or parse failure.
    pub fn lookup(
        &self,
        target_id: &str,
        node: &AIRNode,
        production_only_pinned: bool,
    ) -> Result<Option<Rule>, RuleCacheError> {
        let kind = node_kind_name(&node.kind);
        let rules = self.load_for_target(target_id)?;
        let best = rules
            .into_iter()
            .filter(|r| r.node_kind == kind)
            .filter(|r| !production_only_pinned || r.pinned)
            .max_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then(a.pinned.cmp(&b.pinned))
                    .then(a.created.cmp(&b.created))
            });
        Ok(best)
    }
}

// ─── Node-kind discriminant name ─────────────────────────────────────────────

/// Short discriminant name for a [`NodeKind`]. Kept in sync with the
/// `NodeKind` variants; used as the cache key dimension for lookup.
#[must_use]
pub fn node_kind_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Module { .. } => "Module",
        NodeKind::ImportDecl { .. } => "ImportDecl",
        NodeKind::FnDecl { .. } => "FnDecl",
        NodeKind::RecordDecl { .. } => "RecordDecl",
        NodeKind::EnumDecl { .. } => "EnumDecl",
        NodeKind::ClassDecl { .. } => "ClassDecl",
        NodeKind::TraitDecl { .. } => "TraitDecl",
        NodeKind::ImplBlock { .. } => "ImplBlock",
        NodeKind::EffectDecl { .. } => "EffectDecl",
        NodeKind::ConstDecl { .. } => "ConstDecl",
        NodeKind::TypeAlias { .. } => "TypeAlias",
        NodeKind::Param { .. } => "Param",
        NodeKind::Block { .. } => "Block",
        NodeKind::If { .. } => "If",
        NodeKind::For { .. } => "For",
        NodeKind::While { .. } => "While",
        NodeKind::Loop { .. } => "Loop",
        NodeKind::Match { .. } => "Match",
        NodeKind::MatchArm { .. } => "MatchArm",
        NodeKind::Guard { .. } => "Guard",
        NodeKind::HandlingBlock { .. } => "HandlingBlock",
        NodeKind::LetBinding { .. } => "LetBinding",
        NodeKind::Return { .. } => "Return",
        NodeKind::Break { .. } => "Break",
        NodeKind::Assign { .. } => "Assign",
        NodeKind::BinaryOp { .. } => "BinaryOp",
        NodeKind::UnaryOp { .. } => "UnaryOp",
        NodeKind::Call { .. } => "Call",
        NodeKind::MethodCall { .. } => "MethodCall",
        NodeKind::Lambda { .. } => "Lambda",
        NodeKind::FieldAccess { .. } => "FieldAccess",
        NodeKind::Index { .. } => "Index",
        NodeKind::Pipe { .. } => "Pipe",
        NodeKind::Compose { .. } => "Compose",
        NodeKind::Await { .. } => "Await",
        NodeKind::Propagate { .. } => "Propagate",
        NodeKind::Move { .. } => "Move",
        NodeKind::Borrow { .. } => "Borrow",
        NodeKind::MutableBorrow { .. } => "MutableBorrow",
        NodeKind::ListLiteral { .. } => "ListLiteral",
        NodeKind::SetLiteral { .. } => "SetLiteral",
        NodeKind::TupleLiteral { .. } => "TupleLiteral",
        NodeKind::MapLiteral { .. } => "MapLiteral",
        NodeKind::RecordConstruct { .. } => "RecordConstruct",
        NodeKind::Range { .. } => "Range",
        NodeKind::ResultConstruct { .. } => "ResultConstruct",
        NodeKind::TypeNamed { .. } => "TypeNamed",
        NodeKind::TypeTuple { .. } => "TypeTuple",
        NodeKind::TypeFunction { .. } => "TypeFunction",
        NodeKind::TypeOptional { .. } => "TypeOptional",
        NodeKind::ModuleHandle { .. } => "ModuleHandle",
        NodeKind::PropertyTest { .. } => "PropertyTest",
        NodeKind::ConstructorPat { .. } => "ConstructorPat",
        NodeKind::RecordPat { .. } => "RecordPat",
        NodeKind::TuplePat { .. } => "TuplePat",
        NodeKind::ListPat { .. } => "ListPat",
        NodeKind::OrPat { .. } => "OrPat",
        NodeKind::GuardPat { .. } => "GuardPat",
        NodeKind::RangePat { .. } => "RangePat",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{NodeIdGen, NodeKind};
    use bock_errors::Span;

    fn match_node() -> AIRNode {
        let gen = NodeIdGen::new();
        let scrutinee = AIRNode::new(
            gen.next(),
            Span::dummy(),
            NodeKind::Block {
                stmts: Vec::new(),
                tail: None,
            },
        );
        AIRNode::new(
            gen.next(),
            Span::dummy(),
            NodeKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: Vec::new(),
            },
        )
    }

    fn candidate() -> CandidateRule {
        CandidateRule {
            target_id: "js".into(),
            pattern: "match on string scrutinee".into(),
            template: "switch ({{ scrutinee }}) { {{ arms }} }".into(),
            priority: 10,
        }
    }

    #[test]
    fn candidate_lifts_to_extracted_rule() {
        let rule = Rule::from_candidate(&candidate(), "Match", 0.88);
        assert_eq!(rule.provenance, Provenance::Extracted);
        assert_eq!(rule.node_kind, "Match");
        assert_eq!(rule.target_id, "js");
        assert!(!rule.pinned);
        assert!((rule.confidence - 0.88).abs() < f64::EPSILON);
    }

    #[test]
    fn rule_id_is_stable_across_calls() {
        let a = compute_rule_id("js", "Match", "switch x {}");
        let b = compute_rule_id("js", "Match", "switch x {}");
        assert_eq!(a, b);
        let c = compute_rule_id("js", "Match", "switch y {}");
        assert_ne!(a, c);
    }

    #[test]
    fn insert_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        let rule = Rule::from_candidate(&candidate(), "Match", 0.9);
        cache.insert(&rule).unwrap();

        let loaded = cache.load_for_target("js").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, rule.id);
        assert_eq!(loaded[0].node_kind, "Match");
    }

    #[test]
    fn lookup_matches_by_node_kind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        let rule = Rule::from_candidate(&candidate(), "Match", 0.9);
        cache.insert(&rule).unwrap();

        let hit = cache.lookup("js", &match_node(), false).unwrap();
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().node_kind, "Match");
    }

    #[test]
    fn lookup_misses_on_different_kind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        let rule = Rule::from_candidate(&candidate(), "Call", 0.9);
        cache.insert(&rule).unwrap();

        let hit = cache.lookup("js", &match_node(), false).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn production_mode_ignores_unpinned_rules() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        let rule = Rule::from_candidate(&candidate(), "Match", 0.9);
        cache.insert(&rule).unwrap();

        assert!(cache.lookup("js", &match_node(), true).unwrap().is_none());

        let mut pinned = rule.clone();
        pinned.pinned = true;
        pinned.id = format!("{}-pinned", rule.id);
        cache.insert(&pinned).unwrap();

        let hit = cache.lookup("js", &match_node(), true).unwrap().unwrap();
        assert!(hit.pinned);
    }

    #[test]
    fn lookup_misses_on_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        let hit = cache.lookup("js", &match_node(), false).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn load_skips_non_json_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());
        fs::create_dir_all(cache.target_dir("js")).unwrap();
        fs::write(cache.target_dir("js").join("junk.txt"), "not json").unwrap();
        let rules = cache.load_for_target("js").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn priority_breaks_ties() {
        let dir = tempfile::tempdir().unwrap();
        let cache = RuleCache::new(dir.path());

        let low = Rule {
            id: "low".into(),
            target_id: "js".into(),
            node_kind: "Match".into(),
            pattern: "low".into(),
            template: "low()".into(),
            provenance: Provenance::Extracted,
            pinned: false,
            confidence: 0.5,
            priority: 1,
            created: Utc::now(),
        };
        let high = Rule {
            id: "high".into(),
            priority: 99,
            template: "high()".into(),
            ..low.clone()
        };
        cache.insert(&low).unwrap();
        cache.insert(&high).unwrap();

        let hit = cache.lookup("js", &match_node(), false).unwrap().unwrap();
        assert_eq!(hit.id, "high");
    }

    #[test]
    fn node_kind_name_covers_common_variants() {
        let gen = NodeIdGen::new();
        let block = AIRNode::new(
            gen.next(),
            Span::dummy(),
            NodeKind::Block {
                stmts: Vec::new(),
                tail: None,
            },
        );
        assert_eq!(node_kind_name(&block.kind), "Block");
    }
}
