//! Context composition pass — inherits module context to declarations,
//! computes PII-tainted type sets, and detects security violations across
//! module boundaries.
//!
//! This pass runs after [`crate::context::interpret_context`] and
//! [`crate::validate_context::validate_context`]. It handles:
//!
//! 1. **Context inheritance**: module-level annotations propagate to declarations.
//! 2. **PII-tainted type set**: transitive closure over record fields and generics.
//! 3. **Signature checks**: functions referencing PII types in non-confidential modules.
//! 4. **Cross-module import analysis**: PII-returning imports into non-confidential modules.
//! 5. **Log/print leak detection**: PII types passed to logging/output functions.

use std::collections::{HashMap, HashSet};

use bock_ast::Visibility;
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};

use crate::node::{AIRNode, NodeKind};
use crate::stubs::{ContextBlock, SecurityInfo};

// ─── Public API ──────────────────────────────────────────────────────────────

/// Compose context across one or more modules.
///
/// This pass:
/// 1. Inherits module-level context to child declarations (override semantics,
///    except `@requires` which is additive).
/// 2. Builds the PII-tainted type set transitively (fields, generics).
/// 3. Warns if a function with a PII-tainted signature lives in a non-confidential module.
/// 4. Warns if a PII-returning function is imported into a non-confidential module.
/// 5. Warns if a PII-tainted type appears as an argument to print/log/Log-effect functions.
///
/// Returns a [`DiagnosticBag`] with any warnings or errors.
#[must_use]
pub fn compose_context(modules: &mut [&mut AIRNode]) -> DiagnosticBag {
    let mut diags = DiagnosticBag::new();

    // Step 1: Inherit module context to declarations in each module.
    for module in modules.iter_mut() {
        inherit_module_context(module);
    }

    // Step 2: Build the PII-tainted type set across all modules.
    let pii_set = build_pii_tainted_set(modules);

    // Step 3: Check function signatures for PII types in non-confidential modules.
    for module in modules.iter() {
        check_pii_signatures(module, &pii_set, &mut diags);
    }

    // Step 4: Cross-module import analysis.
    let export_map = build_export_map(modules, &pii_set);
    for module in modules.iter() {
        check_cross_module_imports(module, &export_map, &mut diags);
    }

    // Step 5: Log/print leak detection.
    for module in modules.iter() {
        check_log_leak(module, &pii_set, &mut diags);
    }

    diags
}

// ─── Step 1: Context Inheritance ─────────────────────────────────────────────

/// Inherit module-level context to all declarations within the module.
///
/// Rules:
/// - Declaration-level annotations override module-level for the same kind.
/// - `@requires` is additive: declaration capabilities union with module capabilities.
/// - If a declaration has no context at all, it inherits the full module context.
fn inherit_module_context(module: &mut AIRNode) {
    let module_ctx = module.context.clone();
    let Some(module_ctx) = module_ctx else {
        return;
    };

    if let NodeKind::Module { items, .. } = &mut module.kind {
        for item in items.iter_mut() {
            inherit_to_declaration(item, &module_ctx);
        }
    }
}

/// Apply module context inheritance to a single declaration.
fn inherit_to_declaration(node: &mut AIRNode, module_ctx: &ContextBlock) {
    match &node.kind {
        NodeKind::FnDecl { .. }
        | NodeKind::RecordDecl { .. }
        | NodeKind::EnumDecl { .. }
        | NodeKind::ClassDecl { .. }
        | NodeKind::TraitDecl { .. }
        | NodeKind::ImplBlock { .. }
        | NodeKind::EffectDecl { .. }
        | NodeKind::TypeAlias { .. }
        | NodeKind::ConstDecl { .. } => {
            if let Some(ref mut decl_ctx) = node.context {
                // Declaration has its own context — apply selective inheritance.
                // @requires is additive: union capabilities.
                for cap in &module_ctx.capabilities {
                    decl_ctx.capabilities.insert(cap.clone());
                }
                // Other annotations: declaration overrides if present, else inherit.
                if decl_ctx.context_text.is_none() {
                    decl_ctx.context_text.clone_from(&module_ctx.context_text);
                }
                if decl_ctx.markers.is_empty() && !module_ctx.markers.is_empty() {
                    decl_ctx.markers.clone_from(&module_ctx.markers);
                }
                if decl_ctx.performance.is_none() {
                    decl_ctx.performance.clone_from(&module_ctx.performance);
                }
                if decl_ctx.invariants.is_empty() && !module_ctx.invariants.is_empty() {
                    decl_ctx.invariants.clone_from(&module_ctx.invariants);
                }
                if decl_ctx.security.is_none() {
                    decl_ctx.security.clone_from(&module_ctx.security);
                }
                if decl_ctx.domains.is_empty() && !module_ctx.domains.is_empty() {
                    decl_ctx.domains.clone_from(&module_ctx.domains);
                }
            } else {
                // No declaration context — inherit everything from module.
                node.context = Some(module_ctx.clone());
            }
        }
        _ => {}
    }

    // Recurse into nested declarations (e.g., methods in class/trait/impl).
    inherit_to_children(node, module_ctx);
}

/// Recurse into child declarations for context inheritance.
fn inherit_to_children(node: &mut AIRNode, _module_ctx: &ContextBlock) {
    match &mut node.kind {
        NodeKind::ClassDecl { methods, .. }
        | NodeKind::TraitDecl { methods, .. }
        | NodeKind::ImplBlock { methods, .. } => {
            // Methods inherit from their parent declaration's context (which already
            // has module context merged in), not directly from the module.
            let parent_ctx = node.context.clone().unwrap_or_default();
            for method in methods.iter_mut() {
                inherit_to_declaration(method, &parent_ctx);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            let parent_ctx = node.context.clone().unwrap_or_default();
            for op in operations.iter_mut() {
                inherit_to_declaration(op, &parent_ctx);
            }
        }
        _ => {}
    }
}

// ─── Step 2: PII-Tainted Type Set ───────────────────────────────────────────

/// Build the set of type names that are PII-tainted.
///
/// A type is PII-tainted if:
/// - It is directly annotated `@security(pii: true)`, OR
/// - It contains a field whose type is PII-tainted (transitive closure), OR
/// - It is a generic instantiation where any type parameter is PII-tainted.
#[must_use]
pub fn build_pii_tainted_set(modules: &[&mut AIRNode]) -> HashSet<String> {
    let mut pii_set = HashSet::new();
    let mut type_fields: HashMap<String, Vec<String>> = HashMap::new();

    // First pass: collect directly annotated types and type-field relationships.
    for module in modules {
        collect_type_info(module, &mut pii_set, &mut type_fields);
    }

    // Second pass: transitive closure over field references.
    loop {
        let mut changed = false;
        for (type_name, fields) in &type_fields {
            if pii_set.contains(type_name) {
                continue;
            }
            for field_type in fields {
                if pii_set.contains(field_type) {
                    pii_set.insert(type_name.clone());
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }

    pii_set
}

/// Collect type names that are directly PII-annotated and field-type relationships.
fn collect_type_info(
    node: &AIRNode,
    pii_set: &mut HashSet<String>,
    type_fields: &mut HashMap<String, Vec<String>>,
) {
    match &node.kind {
        NodeKind::RecordDecl { name, fields, .. } => {
            // Check if this type is directly PII-annotated.
            if is_pii_annotated(node) {
                pii_set.insert(name.name.clone());
            }
            // Record field type references for transitive closure.
            let field_types: Vec<String> = fields
                .iter()
                .flat_map(|f| extract_type_names_from_ast_type(&f.ty))
                .collect();
            type_fields.insert(name.name.clone(), field_types);
        }
        NodeKind::ClassDecl { name, fields, .. } => {
            if is_pii_annotated(node) {
                pii_set.insert(name.name.clone());
            }
            let field_types: Vec<String> = fields
                .iter()
                .flat_map(|f| extract_type_names_from_ast_type(&f.ty))
                .collect();
            type_fields.insert(name.name.clone(), field_types);
        }
        NodeKind::EnumDecl { name, variants, .. } => {
            if is_pii_annotated(node) {
                pii_set.insert(name.name.clone());
            }
            // Collect field types from all variant payloads.
            let mut variant_types = Vec::new();
            for variant in variants {
                if let NodeKind::EnumVariant { payload, .. } = &variant.kind {
                    match payload {
                        crate::node::EnumVariantPayload::Struct(fields) => {
                            for f in fields {
                                variant_types.extend(extract_type_names_from_ast_type(&f.ty));
                            }
                        }
                        crate::node::EnumVariantPayload::Tuple(elems) => {
                            for elem in elems {
                                variant_types.extend(extract_type_names_from_air_type(elem));
                            }
                        }
                        crate::node::EnumVariantPayload::Unit => {}
                    }
                }
            }
            type_fields.insert(name.name.clone(), variant_types);
        }
        NodeKind::Module { items, .. } => {
            for item in items {
                collect_type_info(item, pii_set, type_fields);
            }
        }
        _ => {}
    }
}

/// Check if an AIR node has `@security(pii: true)` in its context.
fn is_pii_annotated(node: &AIRNode) -> bool {
    node.context
        .as_ref()
        .and_then(|c| c.security.as_ref())
        .is_some_and(|s| s.pii)
}

/// Extract type names from an AST TypeExpr.
fn extract_type_names_from_ast_type(ty: &bock_ast::TypeExpr) -> Vec<String> {
    let mut names = Vec::new();
    match ty {
        bock_ast::TypeExpr::Named { path, args, .. } => {
            if let Some(seg) = path.segments.last() {
                names.push(seg.name.clone());
            }
            for arg in args {
                names.extend(extract_type_names_from_ast_type(arg));
            }
        }
        bock_ast::TypeExpr::Tuple { elems, .. } => {
            for elem in elems {
                names.extend(extract_type_names_from_ast_type(elem));
            }
        }
        bock_ast::TypeExpr::Function { params, ret, .. } => {
            for p in params {
                names.extend(extract_type_names_from_ast_type(p));
            }
            names.extend(extract_type_names_from_ast_type(ret));
        }
        bock_ast::TypeExpr::Optional { inner, .. } => {
            names.extend(extract_type_names_from_ast_type(inner));
        }
        bock_ast::TypeExpr::SelfType { .. } => {}
    }
    names
}

/// Extract type names from an AIR type-expression node.
fn extract_type_names_from_air_type(node: &AIRNode) -> Vec<String> {
    let mut names = Vec::new();
    match &node.kind {
        NodeKind::TypeNamed { path, args, .. } => {
            if let Some(seg) = path.segments.last() {
                names.push(seg.name.clone());
            }
            for arg in args {
                names.extend(extract_type_names_from_air_type(arg));
            }
        }
        NodeKind::TypeTuple { elems, .. } => {
            for elem in elems {
                names.extend(extract_type_names_from_air_type(elem));
            }
        }
        NodeKind::TypeFunction { params, ret, .. } => {
            for p in params {
                names.extend(extract_type_names_from_air_type(p));
            }
            names.extend(extract_type_names_from_air_type(ret));
        }
        NodeKind::TypeOptional { inner, .. } => {
            names.extend(extract_type_names_from_air_type(inner));
        }
        _ => {}
    }
    names
}

// ─── Step 3: PII Signature Checks ───────────────────────────────────────────

/// Check function signatures for PII types in non-confidential modules.
fn check_pii_signatures(module: &AIRNode, pii_set: &HashSet<String>, diags: &mut DiagnosticBag) {
    let module_security = module.context.as_ref().and_then(|c| c.security.as_ref());

    if let NodeKind::Module { items, .. } = &module.kind {
        for item in items {
            check_item_pii_signature(item, module_security, pii_set, diags);
        }
    }
}

/// Check a single item's function signatures for PII references.
fn check_item_pii_signature(
    node: &AIRNode,
    module_security: Option<&SecurityInfo>,
    pii_set: &HashSet<String>,
    diags: &mut DiagnosticBag,
) {
    match &node.kind {
        NodeKind::FnDecl {
            params,
            return_type,
            name,
            ..
        } => {
            let sig_types = collect_fn_signature_types(params, return_type.as_deref());
            if sig_types.iter().any(|t| pii_set.contains(t)) {
                // Check if the effective security acknowledges PII.
                let effective_security = node
                    .context
                    .as_ref()
                    .and_then(|c| c.security.as_ref())
                    .or(module_security);

                if !security_acknowledges_pii(effective_security) {
                    diags.warning(
                        DiagnosticCode {
                            prefix: 'W',
                            number: 8020,
                        },
                        format!(
                            "function `{}` has PII-tainted types in its signature but its \
                             module lacks a security context acknowledging PII \
                             (e.g., @security(level: \"confidential\") or @security(pii: true))",
                            name.name
                        ),
                        node.span,
                    );
                }
            }
        }
        NodeKind::ClassDecl { methods, .. }
        | NodeKind::TraitDecl { methods, .. }
        | NodeKind::ImplBlock { methods, .. } => {
            for method in methods {
                check_item_pii_signature(method, module_security, pii_set, diags);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            for op in operations {
                check_item_pii_signature(op, module_security, pii_set, diags);
            }
        }
        _ => {}
    }
}

/// Collect type names referenced in a function's parameter and return types.
fn collect_fn_signature_types(params: &[AIRNode], return_type: Option<&AIRNode>) -> Vec<String> {
    let mut types = Vec::new();
    for param in params {
        if let NodeKind::Param { ty, .. } = &param.kind {
            if let Some(ty_node) = ty.as_ref() {
                types.extend(extract_type_names_from_air_type(ty_node));
            }
        }
    }
    if let Some(ret) = return_type {
        types.extend(extract_type_names_from_air_type(ret));
    }
    types
}

/// Check if a security context acknowledges PII.
///
/// A security context acknowledges PII if:
/// - `pii: true` is set, OR
/// - level is `"confidential"` or `"secret"` (sensitive enough to handle PII).
fn security_acknowledges_pii(security: Option<&SecurityInfo>) -> bool {
    match security {
        Some(sec) => sec.pii || sec.level == "confidential" || sec.level == "secret",
        None => false,
    }
}

// ─── Step 4: Cross-Module Import Analysis ────────────────────────────────────

/// Info about an exported function that returns/uses PII types.
#[derive(Debug, Clone)]
struct PiiExport {
    /// The function name.
    fn_name: String,
    /// The source module path.
    module_path: String,
}

/// Build a map of module_path → list of PII-tainted exports.
fn build_export_map(
    modules: &[&mut AIRNode],
    pii_set: &HashSet<String>,
) -> HashMap<String, Vec<PiiExport>> {
    let mut export_map: HashMap<String, Vec<PiiExport>> = HashMap::new();

    for module in modules {
        let module_path = extract_module_path(module);
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                if let NodeKind::FnDecl {
                    visibility: Visibility::Public,
                    name,
                    params,
                    return_type,
                    ..
                } = &item.kind
                {
                    let sig_types = collect_fn_signature_types(params, return_type.as_deref());
                    if sig_types.iter().any(|t| pii_set.contains(t)) {
                        export_map
                            .entry(module_path.clone())
                            .or_default()
                            .push(PiiExport {
                                fn_name: name.name.clone(),
                                module_path: module_path.clone(),
                            });
                    }
                }
            }
        }
    }

    export_map
}

/// Extract the module path string from a module node.
fn extract_module_path(module: &AIRNode) -> String {
    if let NodeKind::Module { path: Some(p), .. } = &module.kind {
        p.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".")
    } else {
        String::new()
    }
}

/// Check cross-module imports for PII-returning functions imported into
/// non-confidential modules.
fn check_cross_module_imports(
    module: &AIRNode,
    export_map: &HashMap<String, Vec<PiiExport>>,
    diags: &mut DiagnosticBag,
) {
    let module_security = module.context.as_ref().and_then(|c| c.security.as_ref());

    if security_acknowledges_pii(module_security) {
        return; // Module already acknowledges PII, no warnings needed.
    }

    if let NodeKind::Module { imports, .. } = &module.kind {
        for import in imports {
            if let NodeKind::ImportDecl { path, items } = &import.kind {
                let import_path = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");

                if let Some(pii_exports) = export_map.get(&import_path) {
                    // Check which imported names match PII exports.
                    let imported_names = get_imported_names(items);
                    for export in pii_exports {
                        if imported_names.contains(&export.fn_name) || imported_names.is_empty() {
                            // Wildcard import or specific import of PII function.
                            diags.warning(
                                DiagnosticCode {
                                    prefix: 'W',
                                    number: 8021,
                                },
                                format!(
                                    "importing PII-returning function `{}` from module `{}` \
                                     into a module without a security context acknowledging PII",
                                    export.fn_name, export.module_path
                                ),
                                import.span,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Extract the set of imported names from an import items list.
fn get_imported_names(items: &bock_ast::ImportItems) -> HashSet<String> {
    match items {
        bock_ast::ImportItems::Named(names) => names.iter().map(|n| n.name.name.clone()).collect(),
        // Module import brings the whole module — treat like wildcard.
        bock_ast::ImportItems::Glob | bock_ast::ImportItems::Module => HashSet::new(), // Empty = wildcard/module import
    }
}

// ─── Step 5: Log/Print Leak Detection ────────────────────────────────────────

/// Well-known logging/output function names.
const LOG_FUNCTIONS: &[&str] = &["print", "println", "log"];

/// Check for PII-tainted types passed to print/log/Log-effect functions.
fn check_log_leak(module: &AIRNode, pii_set: &HashSet<String>, diags: &mut DiagnosticBag) {
    check_log_leak_node(module, pii_set, diags);
}

/// Recursively check for log leak in a node and its children.
fn check_log_leak_node(node: &AIRNode, pii_set: &HashSet<String>, diags: &mut DiagnosticBag) {
    match &node.kind {
        NodeKind::Call { callee, args, .. } => {
            let callee_name = extract_callee_name(callee);
            let is_log_fn = callee_name
                .as_ref()
                .is_some_and(|n| LOG_FUNCTIONS.contains(&n.as_str()));
            if is_log_fn {
                check_args_for_pii(args, pii_set, node.span, diags);
            }
            // Recurse into callee and args.
            check_log_leak_node(callee, pii_set, diags);
            for arg in args {
                check_log_leak_node(&arg.value, pii_set, diags);
            }
        }
        NodeKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            if LOG_FUNCTIONS.contains(&method.name.as_str()) {
                check_args_for_pii(args, pii_set, node.span, diags);
            }
            check_log_leak_node(receiver, pii_set, diags);
            for arg in args {
                check_log_leak_node(&arg.value, pii_set, diags);
            }
        }
        NodeKind::EffectOp { effect, args, .. } => {
            // Check if the effect is a Log effect.
            let is_log_effect = effect.segments.last().is_some_and(|s| s.name == "Log");
            if is_log_effect {
                check_args_for_pii(args, pii_set, node.span, diags);
            }
            for arg in args {
                check_log_leak_node(&arg.value, pii_set, diags);
            }
        }
        // Recurse into all child nodes.
        _ => {
            visit_children_for_log_leak(node, pii_set, diags);
        }
    }
}

/// Check if any arguments reference PII-tainted types.
fn check_args_for_pii(
    args: &[crate::node::AirArg],
    pii_set: &HashSet<String>,
    span: Span,
    diags: &mut DiagnosticBag,
) {
    for arg in args {
        let arg_types = extract_expr_type_refs(&arg.value);
        for type_name in &arg_types {
            if pii_set.contains(type_name) {
                diags.warning(
                    DiagnosticCode {
                        prefix: 'W',
                        number: 8022,
                    },
                    format!(
                        "PII-tainted type `{type_name}` passed to logging/output function; \
                         this is a potential data leak"
                    ),
                    span,
                );
                return; // One warning per call site is sufficient.
            }
        }
    }
}

/// Extract type references from an expression node (heuristic: look at
/// identifiers and record construct paths that might reference PII types).
fn extract_expr_type_refs(node: &AIRNode) -> Vec<String> {
    let mut refs = Vec::new();
    match &node.kind {
        NodeKind::Identifier { name } => {
            // Capitalized identifiers are likely type references or constructors.
            if name.name.starts_with(|c: char| c.is_uppercase()) {
                refs.push(name.name.clone());
            }
        }
        NodeKind::RecordConstruct { path, .. } => {
            if let Some(seg) = path.segments.last() {
                refs.push(seg.name.clone());
            }
        }
        NodeKind::Call { callee, args, .. } => {
            refs.extend(extract_expr_type_refs(callee));
            for arg in args {
                refs.extend(extract_expr_type_refs(&arg.value));
            }
        }
        _ => {}
    }
    refs
}

/// Extract the callee function name from a call expression's callee node.
fn extract_callee_name(callee: &AIRNode) -> Option<String> {
    match &callee.kind {
        NodeKind::Identifier { name } => Some(name.name.clone()),
        NodeKind::FieldAccess { field, .. } => Some(field.name.clone()),
        _ => None,
    }
}

/// Visit child nodes for log leak detection.
fn visit_children_for_log_leak(
    node: &AIRNode,
    pii_set: &HashSet<String>,
    diags: &mut DiagnosticBag,
) {
    match &node.kind {
        NodeKind::Module { imports, items, .. } => {
            for child in imports.iter().chain(items.iter()) {
                check_log_leak_node(child, pii_set, diags);
            }
        }
        NodeKind::FnDecl { body, params, .. } => {
            for p in params {
                check_log_leak_node(p, pii_set, diags);
            }
            check_log_leak_node(body, pii_set, diags);
        }
        NodeKind::ClassDecl { methods, .. }
        | NodeKind::TraitDecl { methods, .. }
        | NodeKind::ImplBlock { methods, .. } => {
            for m in methods {
                check_log_leak_node(m, pii_set, diags);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            for op in operations {
                check_log_leak_node(op, pii_set, diags);
            }
        }
        NodeKind::Block { stmts, tail, .. } => {
            for stmt in stmts {
                check_log_leak_node(stmt, pii_set, diags);
            }
            if let Some(t) = tail.as_ref() {
                check_log_leak_node(t, pii_set, diags);
            }
        }
        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            check_log_leak_node(condition, pii_set, diags);
            check_log_leak_node(then_block, pii_set, diags);
            if let Some(e) = else_block.as_ref() {
                check_log_leak_node(e, pii_set, diags);
            }
        }
        NodeKind::Match {
            scrutinee, arms, ..
        } => {
            check_log_leak_node(scrutinee, pii_set, diags);
            for arm in arms {
                check_log_leak_node(arm, pii_set, diags);
            }
        }
        NodeKind::MatchArm { body, .. } => {
            check_log_leak_node(body, pii_set, diags);
        }
        NodeKind::For { body, iterable, .. } => {
            check_log_leak_node(iterable, pii_set, diags);
            check_log_leak_node(body, pii_set, diags);
        }
        NodeKind::While {
            condition, body, ..
        } => {
            check_log_leak_node(condition, pii_set, diags);
            check_log_leak_node(body, pii_set, diags);
        }
        NodeKind::Loop { body, .. } => {
            check_log_leak_node(body, pii_set, diags);
        }
        NodeKind::LetBinding { value, .. } => {
            check_log_leak_node(value, pii_set, diags);
        }
        NodeKind::HandlingBlock { body, handlers, .. } => {
            check_log_leak_node(body, pii_set, diags);
            for h in handlers {
                check_log_leak_node(&h.handler, pii_set, diags);
            }
        }
        NodeKind::Return { value: Some(v) } | NodeKind::Break { value: Some(v) } => {
            check_log_leak_node(v, pii_set, diags);
        }
        NodeKind::BinaryOp { left, right, .. } => {
            check_log_leak_node(left, pii_set, diags);
            check_log_leak_node(right, pii_set, diags);
        }
        NodeKind::UnaryOp { operand, .. } => {
            check_log_leak_node(operand, pii_set, diags);
        }
        NodeKind::Assign { target, value, .. } => {
            check_log_leak_node(target, pii_set, diags);
            check_log_leak_node(value, pii_set, diags);
        }
        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            check_log_leak_node(left, pii_set, diags);
            check_log_leak_node(right, pii_set, diags);
        }
        NodeKind::Lambda { body, .. } => {
            check_log_leak_node(body, pii_set, diags);
        }
        NodeKind::Index { object, index } => {
            check_log_leak_node(object, pii_set, diags);
            check_log_leak_node(index, pii_set, diags);
        }
        NodeKind::FieldAccess { object, .. } => {
            check_log_leak_node(object, pii_set, diags);
        }
        NodeKind::Propagate { expr }
        | NodeKind::Await { expr }
        | NodeKind::Move { expr }
        | NodeKind::Borrow { expr }
        | NodeKind::MutableBorrow { expr } => {
            check_log_leak_node(expr, pii_set, diags);
        }
        _ => {}
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::interpret_context;
    use crate::node::{AirArg, NodeIdGen, NodeKind};
    use crate::stubs::{Capability, ContextBlock, SecurityInfo};
    use bock_ast::{Annotation, Ident, ImportItems, Literal, ModulePath, TypePath, Visibility};
    use bock_errors::Span;

    fn test_span() -> Span {
        Span::dummy()
    }

    fn str_expr(s: &str) -> bock_ast::Expr {
        bock_ast::Expr::Literal {
            id: 0,
            span: test_span(),
            lit: Literal::String(s.to_string()),
        }
    }

    fn bool_expr(b: bool) -> bock_ast::Expr {
        bock_ast::Expr::Literal {
            id: 0,
            span: test_span(),
            lit: Literal::Bool(b),
        }
    }

    fn capability_expr(name: &str) -> bock_ast::Expr {
        bock_ast::Expr::FieldAccess {
            id: 0,
            span: test_span(),
            object: Box::new(bock_ast::Expr::Identifier {
                id: 0,
                span: test_span(),
                name: Ident {
                    name: "Capability".to_string(),
                    span: test_span(),
                },
            }),
            field: Ident {
                name: name.to_string(),
                span: test_span(),
            },
        }
    }

    fn ann(name: &str, args: Vec<bock_ast::Expr>) -> Annotation {
        Annotation {
            id: 0,
            span: test_span(),
            name: Ident {
                name: name.to_string(),
                span: test_span(),
            },
            args: args
                .into_iter()
                .map(|e| bock_ast::AnnotationArg {
                    label: None,
                    value: e,
                })
                .collect(),
        }
    }

    fn make_ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: test_span(),
        }
    }

    fn make_type_path(name: &str) -> TypePath {
        TypePath {
            segments: vec![make_ident(name)],
            span: test_span(),
        }
    }

    fn make_module_path(segments: &[&str]) -> ModulePath {
        ModulePath {
            segments: segments.iter().map(|s| make_ident(s)).collect(),
            span: test_span(),
        }
    }

    fn fn_node_with_types(
        id_gen: &NodeIdGen,
        name: &str,
        annotations: Vec<Annotation>,
        visibility: Visibility,
        param_type_names: &[&str],
        return_type_name: Option<&str>,
    ) -> AIRNode {
        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );

        let params: Vec<AIRNode> = param_type_names
            .iter()
            .map(|t| {
                let ty_node = AIRNode::new(
                    id_gen.next(),
                    test_span(),
                    NodeKind::TypeNamed {
                        path: make_type_path(t),
                        args: vec![],
                    },
                );
                AIRNode::new(
                    id_gen.next(),
                    test_span(),
                    NodeKind::Param {
                        pattern: Box::new(AIRNode::new(
                            id_gen.next(),
                            test_span(),
                            NodeKind::BindPat {
                                name: make_ident("arg"),
                                is_mut: false,
                            },
                        )),
                        ty: Some(Box::new(ty_node)),
                        default: None,
                    },
                )
            })
            .collect();

        let return_type = return_type_name.map(|t| {
            Box::new(AIRNode::new(
                id_gen.next(),
                test_span(),
                NodeKind::TypeNamed {
                    path: make_type_path(t),
                    args: vec![],
                },
            ))
        });

        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations,
                visibility,
                is_async: false,
                name: make_ident(name),
                generic_params: vec![],
                params,
                return_type,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn fn_node(
        id_gen: &NodeIdGen,
        name: &str,
        annotations: Vec<Annotation>,
        visibility: Visibility,
    ) -> AIRNode {
        fn_node_with_types(id_gen, name, annotations, visibility, &[], None)
    }

    fn record_node(
        id_gen: &NodeIdGen,
        name: &str,
        annotations: Vec<Annotation>,
        field_types: &[&str],
    ) -> AIRNode {
        let fields: Vec<bock_ast::RecordDeclField> = field_types
            .iter()
            .enumerate()
            .map(|(i, t)| bock_ast::RecordDeclField {
                id: 0,
                span: test_span(),
                name: make_ident(&format!("field_{i}")),
                ty: bock_ast::TypeExpr::Named {
                    id: 0,
                    span: test_span(),
                    path: make_type_path(t),
                    args: vec![],
                },
                default: None,
            })
            .collect();

        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::RecordDecl {
                annotations,
                visibility: Visibility::Public,
                name: make_ident(name),
                generic_params: vec![],
                fields,
            },
        )
    }

    fn module_node(id_gen: &NodeIdGen, path: Option<&[&str]>, items: Vec<AIRNode>) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Module {
                path: path.map(make_module_path),
                annotations: vec![],
                imports: vec![],
                items,
            },
        )
    }

    fn module_with_imports(
        id_gen: &NodeIdGen,
        path: Option<&[&str]>,
        imports: Vec<AIRNode>,
        items: Vec<AIRNode>,
    ) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Module {
                path: path.map(make_module_path),
                annotations: vec![],
                imports,
                items,
            },
        )
    }

    fn import_node(id_gen: &NodeIdGen, path: &[&str], names: &[&str]) -> AIRNode {
        AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::ImportDecl {
                path: make_module_path(path),
                items: ImportItems::Named(
                    names
                        .iter()
                        .map(|n| bock_ast::ImportedName {
                            span: test_span(),
                            name: make_ident(n),
                            alias: None,
                        })
                        .collect(),
                ),
            },
        )
    }

    // ── Context Inheritance Tests ─────────────────────────────────────────────

    #[test]
    fn module_context_inherited_by_declaration_without_context() {
        let id_gen = NodeIdGen::new();
        let child = fn_node(&id_gen, "my_fn", vec![], Visibility::Public);
        let mut module = module_node(&id_gen, None, vec![child]);

        module.context = Some(ContextBlock {
            context_text: Some("Payment module.".to_string()),
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            domains: vec!["payments".to_string()],
            ..Default::default()
        });

        let diags = compose_context(&mut [&mut module]);
        assert_eq!(diags.error_count(), 0);

        if let NodeKind::Module { items, .. } = &module.kind {
            let child_ctx = items[0]
                .context
                .as_ref()
                .expect("child should inherit context");
            assert_eq!(child_ctx.context_text.as_deref(), Some("Payment module."));
            assert_eq!(child_ctx.security.as_ref().unwrap().level, "confidential");
            assert!(child_ctx.security.as_ref().unwrap().pii);
            assert_eq!(child_ctx.domains, vec!["payments"]);
        }
    }

    #[test]
    fn declaration_context_overrides_module_context() {
        let id_gen = NodeIdGen::new();
        let mut child = fn_node(
            &id_gen,
            "my_fn",
            vec![
                ann("security", vec![str_expr("secret"), bool_expr(true)]),
                ann("domain", vec![str_expr("billing")]),
            ],
            Visibility::Public,
        );
        let _ = interpret_context(&mut child);

        let mut module = module_node(&id_gen, None, vec![child]);
        module.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: false,
            }),
            domains: vec!["payments".to_string()],
            ..Default::default()
        });

        let diags = compose_context(&mut [&mut module]);
        assert_eq!(diags.error_count(), 0);

        if let NodeKind::Module { items, .. } = &module.kind {
            let child_ctx = items[0].context.as_ref().unwrap();
            // Declaration security overrides module security.
            assert_eq!(child_ctx.security.as_ref().unwrap().level, "secret");
            assert!(child_ctx.security.as_ref().unwrap().pii);
            // Declaration domain overrides module domain.
            assert_eq!(child_ctx.domains, vec!["billing"]);
        }
    }

    #[test]
    fn capabilities_additive_with_module() {
        let id_gen = NodeIdGen::new();
        let mut child = fn_node(
            &id_gen,
            "my_fn",
            vec![ann("requires", vec![capability_expr("Crypto")])],
            Visibility::Public,
        );
        let _ = interpret_context(&mut child);

        let mut module = module_node(&id_gen, None, vec![child]);
        module.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s
            },
            ..Default::default()
        });

        let diags = compose_context(&mut [&mut module]);
        assert_eq!(diags.error_count(), 0);

        if let NodeKind::Module { items, .. } = &module.kind {
            let child_ctx = items[0].context.as_ref().unwrap();
            // Both module (Network) and declaration (Crypto) capabilities present.
            assert!(child_ctx.capabilities.contains(&Capability::new("Network")));
            assert!(child_ctx.capabilities.contains(&Capability::new("Crypto")));
        }
    }

    // ── PII-Tainted Type Set Tests ──────────────────────────────────────────

    #[test]
    fn pii_tainted_set_direct_annotation() {
        let id_gen = NodeIdGen::new();
        let mut record = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String", "String"],
        );
        let _ = interpret_context(&mut record);

        let mut module = module_node(&id_gen, None, vec![record]);
        let pii_set = build_pii_tainted_set(&[&mut module]);
        assert!(pii_set.contains("UserProfile"));
    }

    #[test]
    fn pii_tainted_set_transitive_through_fields() {
        let id_gen = NodeIdGen::new();

        // UserProfile is directly PII.
        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // Order has a field of type UserProfile — should become PII-tainted.
        let order = record_node(&id_gen, "Order", vec![], &["UserProfile", "Int"]);

        let mut module = module_node(&id_gen, None, vec![user, order]);
        let pii_set = build_pii_tainted_set(&[&mut module]);
        assert!(pii_set.contains("UserProfile"));
        assert!(pii_set.contains("Order"));
    }

    #[test]
    fn pii_tainted_set_transitive_chain() {
        let id_gen = NodeIdGen::new();

        // Address is PII.
        let mut address = record_node(
            &id_gen,
            "Address",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut address);

        // Customer contains Address.
        let customer = record_node(&id_gen, "Customer", vec![], &["Address", "String"]);

        // Invoice contains Customer.
        let invoice = record_node(&id_gen, "Invoice", vec![], &["Customer", "Int"]);

        let mut module = module_node(&id_gen, None, vec![address, customer, invoice]);
        let pii_set = build_pii_tainted_set(&[&mut module]);
        assert!(pii_set.contains("Address"));
        assert!(pii_set.contains("Customer"));
        assert!(pii_set.contains("Invoice"));
    }

    #[test]
    fn pii_tainted_set_generic_instantiation() {
        let id_gen = NodeIdGen::new();

        // UserProfile is PII.
        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // Record with field of type List[UserProfile] (generic instantiation).
        let list_of_users = {
            let fields = vec![bock_ast::RecordDeclField {
                id: 0,
                span: test_span(),
                name: make_ident("users"),
                ty: bock_ast::TypeExpr::Named {
                    id: 0,
                    span: test_span(),
                    path: make_type_path("List"),
                    args: vec![bock_ast::TypeExpr::Named {
                        id: 0,
                        span: test_span(),
                        path: make_type_path("UserProfile"),
                        args: vec![],
                    }],
                },
                default: None,
            }];
            AIRNode::new(
                id_gen.next(),
                test_span(),
                NodeKind::RecordDecl {
                    annotations: vec![],
                    visibility: Visibility::Public,
                    name: make_ident("UserList"),
                    generic_params: vec![],
                    fields,
                },
            )
        };

        let mut module = module_node(&id_gen, None, vec![user, list_of_users]);
        let pii_set = build_pii_tainted_set(&[&mut module]);
        assert!(pii_set.contains("UserProfile"));
        assert!(
            pii_set.contains("UserList"),
            "UserList should be PII-tainted because it contains List[UserProfile]"
        );
    }

    // ── PII Signature Warning Tests ─────────────────────────────────────────

    #[test]
    fn pii_signature_in_non_confidential_module_warns() {
        let id_gen = NodeIdGen::new();

        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // Function returning UserProfile in a module with no security context.
        let get_user = fn_node_with_types(
            &id_gen,
            "get_user",
            vec![],
            Visibility::Public,
            &[],
            Some("UserProfile"),
        );

        let mut module = module_node(&id_gen, None, vec![user, get_user]);
        // No security context on module.

        let diags = compose_context(&mut [&mut module]);
        assert!(
            diags.warning_count() > 0,
            "should warn about PII type in function signature without security context"
        );
    }

    #[test]
    fn pii_signature_in_confidential_module_no_warning() {
        let id_gen = NodeIdGen::new();

        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        let get_user = fn_node_with_types(
            &id_gen,
            "get_user",
            vec![],
            Visibility::Public,
            &[],
            Some("UserProfile"),
        );

        let mut module = module_node(&id_gen, None, vec![user, get_user]);
        module.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        let diags = compose_context(&mut [&mut module]);
        assert_eq!(
            diags.warning_count(),
            0,
            "confidential module should not warn"
        );
    }

    #[test]
    fn pii_param_type_in_non_confidential_module_warns() {
        let id_gen = NodeIdGen::new();

        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // Function with UserProfile parameter.
        let save_user = fn_node_with_types(
            &id_gen,
            "save_user",
            vec![],
            Visibility::Public,
            &["UserProfile"],
            None,
        );

        let mut module = module_node(&id_gen, None, vec![user, save_user]);

        let diags = compose_context(&mut [&mut module]);
        assert!(diags.warning_count() > 0, "should warn on PII param type");
    }

    // ── Cross-Module Import Tests ───────────────────────────────────────────

    #[test]
    fn cross_module_pii_import_into_non_confidential_warns() {
        let id_gen = NodeIdGen::new();

        // Module A: exports a PII-returning function.
        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        let get_user = fn_node_with_types(
            &id_gen,
            "get_user",
            vec![],
            Visibility::Public,
            &[],
            Some("UserProfile"),
        );

        let mut module_a = module_node(&id_gen, Some(&["ModA"]), vec![user, get_user]);
        module_a.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        // Module B: imports get_user from ModA without security context.
        let import = import_node(&id_gen, &["ModA"], &["get_user"]);
        let mut module_b = module_with_imports(&id_gen, Some(&["ModB"]), vec![import], vec![]);
        // No security context on module B.

        let diags = compose_context(&mut [&mut module_a, &mut module_b]);
        assert!(
            diags.warning_count() > 0,
            "should warn about importing PII function into non-confidential module"
        );
    }

    #[test]
    fn cross_module_pii_import_into_confidential_no_warning() {
        let id_gen = NodeIdGen::new();

        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        let get_user = fn_node_with_types(
            &id_gen,
            "get_user",
            vec![],
            Visibility::Public,
            &[],
            Some("UserProfile"),
        );

        let mut module_a = module_node(&id_gen, Some(&["ModA"]), vec![user, get_user]);
        module_a.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        let import = import_node(&id_gen, &["ModA"], &["get_user"]);
        let mut module_b = module_with_imports(&id_gen, Some(&["ModB"]), vec![import], vec![]);
        module_b.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        let diags = compose_context(&mut [&mut module_a, &mut module_b]);
        // Filter to only W8021 warnings (cross-module import warnings).
        assert_eq!(
            diags.warning_count(),
            0,
            "confidential importer should not warn"
        );
    }

    // ── Log/Print Leak Detection Tests ──────────────────────────────────────

    #[test]
    fn pii_type_passed_to_print_warns() {
        let id_gen = NodeIdGen::new();

        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // A function that calls println(UserProfile{...}).
        let print_call = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Call {
                callee: Box::new(AIRNode::new(
                    id_gen.next(),
                    test_span(),
                    NodeKind::Identifier {
                        name: make_ident("println"),
                    },
                )),
                args: vec![AirArg {
                    label: None,
                    value: AIRNode::new(
                        id_gen.next(),
                        test_span(),
                        NodeKind::RecordConstruct {
                            path: make_type_path("UserProfile"),
                            fields: vec![],
                            spread: None,
                        },
                    ),
                }],
                type_args: vec![],
            },
        );

        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![print_call],
                tail: None,
            },
        );

        let leak_fn = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: make_ident("leak_fn"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );

        let mut module = module_node(&id_gen, None, vec![user, leak_fn]);

        let diags = compose_context(&mut [&mut module]);
        // Should have at least a W8022 log leak warning.
        let has_log_leak = diags.warning_count() > 0;
        assert!(has_log_leak, "should warn about PII type passed to println");
    }

    #[test]
    fn pii_type_passed_to_log_effect_warns() {
        let id_gen = NodeIdGen::new();

        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // An effect operation: Log.info(UserProfile{...})
        let log_op = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::EffectOp {
                effect: make_type_path("Log"),
                operation: make_ident("info"),
                args: vec![AirArg {
                    label: None,
                    value: AIRNode::new(
                        id_gen.next(),
                        test_span(),
                        NodeKind::RecordConstruct {
                            path: make_type_path("UserProfile"),
                            fields: vec![],
                            spread: None,
                        },
                    ),
                }],
            },
        );

        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![log_op],
                tail: None,
            },
        );

        let leak_fn = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: make_ident("leak_fn"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );

        let mut module = module_node(&id_gen, None, vec![user, leak_fn]);

        let diags = compose_context(&mut [&mut module]);
        assert!(
            diags.warning_count() > 0,
            "should warn about PII type passed to Log effect"
        );
    }

    #[test]
    fn no_pii_no_warnings() {
        let id_gen = NodeIdGen::new();

        let record = record_node(&id_gen, "Config", vec![], &["String", "Int"]);

        let print_call = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Call {
                callee: Box::new(AIRNode::new(
                    id_gen.next(),
                    test_span(),
                    NodeKind::Identifier {
                        name: make_ident("println"),
                    },
                )),
                args: vec![AirArg {
                    label: None,
                    value: AIRNode::new(
                        id_gen.next(),
                        test_span(),
                        NodeKind::RecordConstruct {
                            path: make_type_path("Config"),
                            fields: vec![],
                            spread: None,
                        },
                    ),
                }],
                type_args: vec![],
            },
        );

        let body = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::Block {
                stmts: vec![print_call],
                tail: None,
            },
        );

        let print_fn = AIRNode::new(
            id_gen.next(),
            test_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: make_ident("print_config"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );

        let mut module = module_node(&id_gen, None, vec![record, print_fn]);

        let diags = compose_context(&mut [&mut module]);
        assert_eq!(diags.warning_count(), 0, "non-PII types should not warn");
        assert_eq!(diags.error_count(), 0);
    }

    // ── Transitive Context Composition Test ─────────────────────────────────

    #[test]
    fn context_composition_transitive() {
        // Module A declares PII type and exports function.
        // Module B imports from A.
        // This verifies that the PII-tainted set propagates across modules.
        let id_gen = NodeIdGen::new();

        // Module A with PII type.
        let mut user = record_node(
            &id_gen,
            "UserProfile",
            vec![ann(
                "security",
                vec![str_expr("confidential"), bool_expr(true)],
            )],
            &["String"],
        );
        let _ = interpret_context(&mut user);

        // Module A has a type that references UserProfile.
        let user_response = record_node(&id_gen, "UserResponse", vec![], &["UserProfile", "Int"]);

        // Module A exports a function returning UserResponse.
        let get_response = fn_node_with_types(
            &id_gen,
            "get_response",
            vec![],
            Visibility::Public,
            &[],
            Some("UserResponse"),
        );

        let mut module_a = module_node(
            &id_gen,
            Some(&["ModA"]),
            vec![user, user_response, get_response],
        );
        module_a.context = Some(ContextBlock {
            security: Some(SecurityInfo {
                level: "confidential".to_string(),
                pii: true,
            }),
            ..Default::default()
        });

        // Module B imports get_response without security.
        let import = import_node(&id_gen, &["ModA"], &["get_response"]);
        let mut module_b = module_with_imports(&id_gen, Some(&["ModB"]), vec![import], vec![]);

        let diags = compose_context(&mut [&mut module_a, &mut module_b]);
        assert!(
            diags.warning_count() > 0,
            "transitive PII should produce cross-module warning"
        );
    }

    #[test]
    fn capability_propagation_cross_module() {
        // Verify that module-level capabilities are inherited by all declarations.
        let id_gen = NodeIdGen::new();

        let child1 = fn_node(&id_gen, "fn_a", vec![], Visibility::Public);
        let child2 = fn_node(&id_gen, "fn_b", vec![], Visibility::Private);

        let mut module = module_node(&id_gen, None, vec![child1, child2]);
        module.context = Some(ContextBlock {
            capabilities: {
                let mut s = HashSet::new();
                s.insert(Capability::new("Network"));
                s.insert(Capability::new("Storage"));
                s
            },
            ..Default::default()
        });

        let diags = compose_context(&mut [&mut module]);
        assert_eq!(diags.error_count(), 0);

        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                let ctx = item
                    .context
                    .as_ref()
                    .expect("should inherit module context");
                assert!(ctx.capabilities.contains(&Capability::new("Network")));
                assert!(ctx.capabilities.contains(&Capability::new("Storage")));
            }
        }
    }
}
