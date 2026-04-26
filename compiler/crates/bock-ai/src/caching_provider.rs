//! Provider wrapper that adds content-addressed response caching
//! around any [`AiProvider`] implementation.
//!
//! Each call's request is reduced to a deterministic JSON key (mode +
//! model identifier + the cacheable surface of the request) and looked
//! up in an [`AiCache`]. On a hit, the cached response is returned
//! immediately; on a miss the inner provider is called and its
//! response is stored under the same key.
//!
//! Per §17.8, cached responses are treated as **pinned** — the
//! `from_cache: true` signal returned by the
//! [`*_cached`](Self::generate_cached) inherent methods lets the
//! decision-recording layer set [`Decision::pinned`](
//! crate::decision::Decision::pinned) accordingly. The plain
//! [`AiProvider`] impl drops the signal so the wrapper is a drop-in
//! replacement for any existing provider.

use async_trait::async_trait;
use serde::Serialize;

use crate::cache::AiCache;
use crate::error::AiError;
use crate::provider::AiProvider;
use crate::request::{
    GenerateRequest, GenerateResponse, OptimizeRequest, OptimizeResponse, RepairRequest,
    RepairResponse, SelectContext, SelectOption, SelectRequest, SelectResponse,
};

/// Provider wrapper that intercepts each call with [`AiCache`].
///
/// Generic in the inner provider so the wrapper can be stacked on top
/// of any [`AiProvider`] without erasing the concrete type.
#[derive(Debug)]
pub struct CachingProvider<P: AiProvider> {
    inner: P,
    cache: AiCache,
}

impl<P: AiProvider> CachingProvider<P> {
    /// Wraps `inner` with the given cache.
    pub fn new(inner: P, cache: AiCache) -> Self {
        Self { inner, cache }
    }

    /// Borrows the underlying cache (e.g., for stats or clear).
    #[must_use]
    pub fn cache(&self) -> &AiCache {
        &self.cache
    }

    /// Returns the inner provider.
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// [`generate`](AiProvider::generate) plus a `from_cache` flag.
    ///
    /// Callers that record decisions use the flag to set
    /// [`Decision::pinned`](crate::decision::Decision::pinned) — see
    /// §17.8: cached responses are treated as pinned.
    ///
    /// # Errors
    /// Forwards any error from the inner provider on a cache miss.
    pub async fn generate_cached(
        &self,
        request: &GenerateRequest,
    ) -> Result<(GenerateResponse, bool), AiError> {
        let key = generate_key(self.inner.model_id(), request);
        if let Some(resp) = self.cache.get::<_, GenerateResponse>(&key) {
            return Ok((resp, true));
        }
        let resp = self.inner.generate(request).await?;
        let _ = self.cache.put(&key, &resp);
        Ok((resp, false))
    }

    /// [`repair`](AiProvider::repair) plus a `from_cache` flag.
    ///
    /// # Errors
    /// Forwards any error from the inner provider on a cache miss.
    pub async fn repair_cached(
        &self,
        request: &RepairRequest,
    ) -> Result<(RepairResponse, bool), AiError> {
        let key = repair_key(self.inner.model_id(), request);
        if let Some(resp) = self.cache.get::<_, RepairResponse>(&key) {
            return Ok((resp, true));
        }
        let resp = self.inner.repair(request).await?;
        let _ = self.cache.put(&key, &resp);
        Ok((resp, false))
    }

    /// [`optimize`](AiProvider::optimize) plus a `from_cache` flag.
    ///
    /// # Errors
    /// Forwards any error from the inner provider on a cache miss.
    pub async fn optimize_cached(
        &self,
        request: &OptimizeRequest,
    ) -> Result<(OptimizeResponse, bool), AiError> {
        let key = optimize_key(self.inner.model_id(), request);
        if let Some(resp) = self.cache.get::<_, OptimizeResponse>(&key) {
            return Ok((resp, true));
        }
        let resp = self.inner.optimize(request).await?;
        let _ = self.cache.put(&key, &resp);
        Ok((resp, false))
    }

    /// [`select`](AiProvider::select) plus a `from_cache` flag.
    ///
    /// # Errors
    /// Forwards any error from the inner provider on a cache miss.
    pub async fn select_cached(
        &self,
        request: &SelectRequest,
    ) -> Result<(SelectResponse, bool), AiError> {
        let key = select_key(self.inner.model_id(), request);
        if let Some(resp) = self.cache.get::<_, SelectResponse>(&key) {
            return Ok((resp, true));
        }
        let resp = self.inner.select(request).await?;
        let _ = self.cache.put(&key, &resp);
        Ok((resp, false))
    }
}

#[async_trait]
impl<P: AiProvider> AiProvider for CachingProvider<P> {
    async fn generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResponse, AiError> {
        self.generate_cached(request).await.map(|(r, _)| r)
    }

    async fn repair(&self, request: &RepairRequest) -> Result<RepairResponse, AiError> {
        self.repair_cached(request).await.map(|(r, _)| r)
    }

    async fn optimize(
        &self,
        request: &OptimizeRequest,
    ) -> Result<OptimizeResponse, AiError> {
        self.optimize_cached(request).await.map(|(r, _)| r)
    }

    async fn select(&self, request: &SelectRequest) -> Result<SelectResponse, AiError> {
        self.select_cached(request).await.map(|(r, _)| r)
    }

    fn model_id(&self) -> String {
        self.inner.model_id()
    }
}

// ─── Cache key construction ──────────────────────────────────────────────────
//
// The request types in `request.rs` carry an `AIRNode` and a
// `Strictness`, neither of which currently implements `Serialize`. The
// cacheable surface is reconstructed below into a `Serialize` shadow
// struct: scalar fields pass through, the AIR node is captured via its
// `Debug` projection (stable for any given node value), and the
// strictness is rendered as a static string.
//
// Adding `Serialize` to `AIRNode` would propagate through `bock-air`,
// `bock-ast`, `bock-source`, and `bock-errors` — out of scope for this
// package. The shadow approach keeps the change contained to
// `bock-ai` and makes the cacheable contract for each mode explicit.

#[derive(Serialize)]
struct GenerateKey<'a> {
    mode: &'a str,
    model_id: String,
    target_id: &'a str,
    module_path: &'a str,
    imports: &'a [String],
    siblings: &'a [String],
    annotations: &'a [String],
    prior_decisions: Vec<(&'a str, &'a str)>,
    strictness: &'static str,
    node_debug: String,
    capabilities: Vec<(&'a String, &'a String)>,
    conventions: Vec<(&'a String, &'a String)>,
}

fn generate_key(model_id: String, req: &GenerateRequest) -> GenerateKey<'_> {
    let prior: Vec<(&str, &str)> = req
        .prior_decisions
        .iter()
        .map(|d| (d.decision.as_str(), d.choice.as_str()))
        .collect();
    GenerateKey {
        mode: "generate",
        model_id,
        target_id: &req.target.id,
        module_path: &req.module_context.module_path,
        imports: &req.module_context.imports,
        siblings: &req.module_context.siblings,
        annotations: &req.module_context.annotations,
        prior_decisions: prior,
        strictness: strictness_str(req.strictness),
        node_debug: format!("{:?}", req.node),
        capabilities: req.target.capabilities.iter().collect(),
        conventions: req.target.conventions.iter().collect(),
    }
}

#[derive(Serialize)]
struct RepairKey<'a> {
    mode: &'a str,
    model_id: String,
    target_id: &'a str,
    original_code: &'a str,
    compiler_error: &'a str,
    node_debug: String,
}

fn repair_key(model_id: String, req: &RepairRequest) -> RepairKey<'_> {
    RepairKey {
        mode: "repair",
        model_id,
        target_id: &req.target.id,
        original_code: &req.original_code,
        compiler_error: &req.compiler_error,
        node_debug: format!("{:?}", req.node),
    }
}

#[derive(Serialize)]
struct OptimizeKey<'a> {
    mode: &'a str,
    model_id: String,
    target_id: &'a str,
    working_code: &'a str,
    node_debug: String,
    optimization_hints: Vec<String>,
}

fn optimize_key(model_id: String, req: &OptimizeRequest) -> OptimizeKey<'_> {
    OptimizeKey {
        mode: "optimize",
        model_id,
        target_id: &req.target.id,
        working_code: &req.working_code,
        node_debug: format!("{:?}", req.node),
        optimization_hints: req
            .optimization_hints
            .iter()
            .map(|h| format!("{h:?}"))
            .collect(),
    }
}

#[derive(Serialize)]
struct SelectKey<'a> {
    mode: &'a str,
    model_id: String,
    rationale_prompt: &'a str,
    options: Vec<(&'a str, &'a str)>,
    context: SelectContextKey<'a>,
}

#[derive(Serialize)]
struct SelectContextKey<'a> {
    error: Option<&'a str>,
    annotations: &'a [String],
    history: &'a [String],
    metadata: Vec<(&'a String, &'a String)>,
}

fn select_key(model_id: String, req: &SelectRequest) -> SelectKey<'_> {
    let options: Vec<(&str, &str)> = req
        .options
        .iter()
        .map(|o: &SelectOption| (o.id.as_str(), o.description.as_str()))
        .collect();
    let ctx: &SelectContext = &req.context;
    SelectKey {
        mode: "select",
        model_id,
        rationale_prompt: &req.rationale_prompt,
        options,
        context: SelectContextKey {
            error: ctx.error.as_deref(),
            annotations: &ctx.annotations,
            history: &ctx.history,
            metadata: ctx.metadata.iter().collect(),
        },
    }
}

fn strictness_str(s: bock_types::Strictness) -> &'static str {
    match s {
        bock_types::Strictness::Sketch => "sketch",
        bock_types::Strictness::Development => "development",
        bock_types::Strictness::Production => "production",
    }
}

// ─── Serialize derives for response types ────────────────────────────────────
//
// The cache stores responses as JSON. The response types live in
// `request.rs`; `Serialize`/`Deserialize` are added there alongside
// the existing derives so the cache is the only consumer of the new
// contract.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{
        Alternative, GenerateRequest, ModuleContext, OptimizationHint, RepairRequest,
        SelectContext, SelectOption, SelectRequest, TargetProfile,
    };
    use crate::StubProvider;
    use bock_air::{AIRNode, NodeIdGen, NodeKind};
    use bock_errors::Span;
    use bock_types::Strictness;
    use std::collections::HashMap;

    fn dummy_node() -> AIRNode {
        let gen = NodeIdGen::new();
        AIRNode::new(
            gen.next(),
            Span::dummy(),
            NodeKind::Block {
                stmts: Vec::new(),
                tail: None,
            },
        )
    }

    fn target() -> TargetProfile {
        TargetProfile {
            id: "js".into(),
            display_name: "JavaScript".into(),
            capabilities: HashMap::new(),
            conventions: HashMap::new(),
        }
    }

    fn gen_req() -> GenerateRequest {
        GenerateRequest {
            node: dummy_node(),
            target: target(),
            module_context: ModuleContext::default(),
            prior_decisions: Vec::new(),
            strictness: Strictness::Development,
        }
    }

    #[tokio::test]
    async fn generate_first_call_misses_then_hits() {
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));
        let req = gen_req();

        let (r1, hit1) = provider.generate_cached(&req).await.unwrap();
        assert!(!hit1, "first call should miss");

        let (r2, hit2) = provider.generate_cached(&req).await.unwrap();
        assert!(hit2, "second call should hit cache");
        assert_eq!(r1, r2);
    }

    #[tokio::test]
    async fn repair_round_trips_through_cache() {
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));
        let req = RepairRequest {
            original_code: "let x = 1;".into(),
            compiler_error: "missing semicolon".into(),
            node: dummy_node(),
            target: target(),
        };
        let (_, h1) = provider.repair_cached(&req).await.unwrap();
        let (_, h2) = provider.repair_cached(&req).await.unwrap();
        assert!(!h1);
        assert!(h2);
    }

    #[tokio::test]
    async fn optimize_round_trips_through_cache() {
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));
        let req = OptimizeRequest {
            working_code: "return 1;".into(),
            node: dummy_node(),
            target: target(),
            optimization_hints: vec![OptimizationHint::Performance],
        };
        let (_, h1) = provider.optimize_cached(&req).await.unwrap();
        let (_, h2) = provider.optimize_cached(&req).await.unwrap();
        assert!(!h1);
        assert!(h2);
    }

    #[tokio::test]
    async fn select_round_trips_through_cache() {
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));
        let req = SelectRequest {
            options: vec![
                SelectOption {
                    id: "retry".into(),
                    description: "retry".into(),
                },
                SelectOption {
                    id: "fallback".into(),
                    description: "fallback".into(),
                },
            ],
            context: SelectContext::default(),
            rationale_prompt: "pick one".into(),
        };
        let (_, h1) = provider.select_cached(&req).await.unwrap();
        let (_, h2) = provider.select_cached(&req).await.unwrap();
        assert!(!h1);
        assert!(h2);
    }

    #[tokio::test]
    async fn trait_impl_drops_from_cache_signal() {
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));
        let req = gen_req();
        // Calling through the trait should also populate and read the
        // cache, just without surfacing the signal.
        let r1 = provider.generate(&req).await.unwrap();
        let r2 = provider.generate(&req).await.unwrap();
        assert_eq!(r1, r2);
        // Underlying cache now has exactly one entry.
        assert_eq!(provider.cache().stats().unwrap().entries, 1);
    }

    #[tokio::test]
    async fn distinct_requests_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));

        let r1 = RepairRequest {
            original_code: "let x = 1;".into(),
            compiler_error: "e1".into(),
            node: dummy_node(),
            target: target(),
        };
        let r2 = RepairRequest {
            original_code: "let x = 2;".into(),
            compiler_error: "e1".into(),
            node: dummy_node(),
            target: target(),
        };

        let (_, h1) = provider.repair_cached(&r1).await.unwrap();
        let (_, h2) = provider.repair_cached(&r2).await.unwrap();
        assert!(!h1);
        assert!(!h2, "different request must miss separately");
        assert_eq!(provider.cache().stats().unwrap().entries, 2);
    }

    #[tokio::test]
    async fn alternative_serializes_through_cache() {
        // Sanity: ensure the response types we cache survive a JSON
        // round trip including their nested Alternative records.
        let dir = tempfile::tempdir().unwrap();
        let provider = CachingProvider::new(StubProvider::default(), AiCache::new(dir.path()));
        let req = gen_req();
        let (resp, _) = provider.generate_cached(&req).await.unwrap();
        let _alt = Alternative {
            label: "rendered".into(),
            reasoning: None,
            confidence: 0.5,
        };
        // Round trip the actual cached response.
        let bytes = serde_json::to_vec(&resp).unwrap();
        let _back: GenerateResponse = serde_json::from_slice(&bytes).unwrap();
    }
}
