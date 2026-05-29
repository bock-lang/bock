//! Implementation of the `bock doc` command.
//!
//! Extracts `///` item doc comments and `//!` module doc comments from
//! Bock source files and produces a reference organized by module.
//! Supports two output formats: markdown (default) and self-contained
//! HTML with a sidebar navigation and cross-module anchor links.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use bock_ast::{
    Annotation, EffectDecl, EnumDecl, EnumVariant, FnDecl, GenericParam, Item, Module, Param,
    RecordDecl, RecordDeclField, TraitDecl, TypeAliasDecl, TypeExpr, TypePath, Visibility,
};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceFile;

/// Output format for `bock doc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    /// One markdown file per module plus a `README.md` index.
    Markdown,
    /// One HTML file per module plus an `index.html` with a sidebar.
    Html,
}

impl DocFormat {
    fn parse(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "md" | "markdown" => Ok(DocFormat::Markdown),
            "html" => Ok(DocFormat::Html),
            other => Err(anyhow::anyhow!(
                "unknown doc format '{other}'; expected 'markdown' or 'html'"
            )),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            DocFormat::Markdown => "md",
            DocFormat::Html => "html",
        }
    }
}

/// A parsed `.bock` file ready for rendering.
struct ParsedModule {
    /// Human-readable name (`module Foo.Bar` → `Foo.Bar`, else file stem).
    name: String,
    /// Slug used for output filenames and links (dots preserved).
    slug: String,
    module: Module,
    source: String,
}

/// Simple index mapping each top-level type name to the module that
/// defines it. Used to emit cross-module anchor links.
struct SymbolIndex {
    /// Type name → owning module slug.
    defs: BTreeMap<String, String>,
}

impl SymbolIndex {
    fn build(modules: &[ParsedModule]) -> Self {
        let mut defs = BTreeMap::new();
        for pm in modules {
            for item in &pm.module.items {
                let name = match item {
                    Item::Record(d) => Some(d.name.name.clone()),
                    Item::Enum(d) => Some(d.name.name.clone()),
                    Item::Trait(d) | Item::PlatformTrait(d) => Some(d.name.name.clone()),
                    Item::Effect(d) => Some(d.name.name.clone()),
                    Item::TypeAlias(d) => Some(d.name.name.clone()),
                    _ => None,
                };
                if let Some(n) = name {
                    defs.entry(n).or_insert_with(|| pm.slug.clone());
                }
            }
        }
        SymbolIndex { defs }
    }

    fn module_of(&self, type_name: &str) -> Option<&str> {
        self.defs.get(type_name).map(String::as_str)
    }
}

/// Project metadata parsed from `bock.project` (best-effort).
struct ProjectMeta {
    name: String,
    version: String,
}

/// Run `bock doc`.
pub fn run(path: Option<String>, output_dir: Option<String>, format: &str) -> anyhow::Result<()> {
    let format = DocFormat::parse(format)?;
    let input = PathBuf::from(path.as_deref().unwrap_or("."));

    let (files, default_out, project_root) = if input.is_dir() {
        let mut files = Vec::new();
        discover_bock_files_recursive(&input, &mut files)?;
        files.sort();
        (files, input.join("docs"), input.clone())
    } else if input.is_file() {
        let parent = input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        (vec![input.clone()], parent.join("docs"), parent)
    } else {
        eprintln!("error: path does not exist: {}", input.display());
        process::exit(1);
    };

    if files.is_empty() {
        eprintln!("No .bock files found.");
        process::exit(1);
    }

    let out_dir = output_dir.map(PathBuf::from).unwrap_or(default_out);
    fs::create_dir_all(&out_dir)?;

    // Parse all modules up-front so that cross-module link resolution
    // has a complete picture before rendering begins.
    let mut parsed: Vec<ParsedModule> = Vec::new();
    for file in &files {
        match parse_file(file) {
            Ok(Some(pm)) => parsed.push(pm),
            Ok(None) => {} // diagnostics were already printed.
            Err(e) => eprintln!("error: {}: {e}", file.display()),
        }
    }

    if parsed.is_empty() {
        eprintln!("No modules to document.");
        process::exit(1);
    }

    // Disambiguate slugs that collide (e.g., two files both declare `main`).
    dedupe_slugs(&mut parsed);

    let index = SymbolIndex::build(&parsed);
    let project = read_project_meta(&project_root);

    let ext = format.extension();
    let mut written = 0usize;
    for pm in &parsed {
        let ctx = RenderCtx {
            format,
            index: &index,
            current_module: &pm.slug,
        };
        let content = match format {
            DocFormat::Markdown => render_module_markdown(pm, &ctx),
            DocFormat::Html => render_module_html(pm, &parsed, &ctx),
        };
        let out_path = out_dir.join(format!("{}.{}", pm.slug, ext));
        fs::write(&out_path, content)?;
        println!("wrote {}", out_path.display());
        written += 1;
    }

    // Index page.
    let index_content = match format {
        DocFormat::Markdown => render_markdown_index(&parsed, project.as_ref()),
        DocFormat::Html => render_html_index(&parsed, project.as_ref()),
    };
    let index_path = out_dir.join(match format {
        DocFormat::Markdown => "README.md",
        DocFormat::Html => "index.html",
    });
    fs::write(&index_path, index_content)?;
    println!("wrote {}", index_path.display());

    println!(
        "doc: {written} module(s) documented into {} (format: {})",
        out_dir.display(),
        match format {
            DocFormat::Markdown => "markdown",
            DocFormat::Html => "html",
        }
    );
    Ok(())
}

fn parse_file(file: &Path) -> anyhow::Result<Option<ParsedModule>> {
    let content = fs::read_to_string(file)?;
    let source = SourceFile::new(bock_errors::FileId(0), file.to_path_buf(), content.clone());
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize();
    if lexer.diagnostics().has_errors() {
        eprintln!("error: lexer errors in {}; skipping", file.display());
        return Ok(None);
    }
    let mut parser = Parser::new(tokens, &source);
    let module = parser.parse_module();
    if parser.diagnostics().has_errors() {
        eprintln!("error: parser errors in {}; skipping", file.display());
        return Ok(None);
    }
    let name = module_name_for(&module, file);
    let slug = slug_for(&name);
    Ok(Some(ParsedModule {
        name,
        slug,
        module,
        source: content,
    }))
}

fn module_name_for(module: &Module, file: &Path) -> String {
    if let Some(path) = &module.path {
        let parts: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
        if !parts.is_empty() {
            return parts.join(".");
        }
    }
    file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string()
}

/// Turn a dotted module name into a filesystem-safe slug.
fn slug_for(name: &str) -> String {
    name.replace(['/', '\\'], "_")
}

/// Ensure no two modules share the same slug by appending `_N`.
fn dedupe_slugs(parsed: &mut [ParsedModule]) {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for pm in parsed.iter_mut() {
        let n = seen.entry(pm.slug.clone()).or_insert(0);
        if *n > 0 {
            pm.slug = format!("{}_{}", pm.slug, *n);
        }
        *n += 1;
    }
}

fn discover_bock_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("could not read directory '{}': {e}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with('.')
                && name_str != "build"
                && name_str != "target"
                && name_str != "node_modules"
                && name_str != "docs"
            {
                discover_bock_files_recursive(&path, files)?;
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension() {
                if ext == "bock" {
                    files.push(path);
                }
            }
        }
    }
    Ok(())
}

fn read_project_meta(dir: &Path) -> Option<ProjectMeta> {
    let path = dir.join("bock.project");
    let content = fs::read_to_string(&path).ok()?;
    let mut in_project = false;
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_project = trimmed == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(rest) = strip_field(trimmed, "name") {
            name = Some(rest);
        } else if let Some(rest) = strip_field(trimmed, "version") {
            version = Some(rest);
        }
    }
    Some(ProjectMeta {
        name: name?,
        version: version?,
    })
}

fn strip_field(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    Some(rest.trim_matches('"').to_string())
}

// ─── Rendering context ────────────────────────────────────────────────────────

struct RenderCtx<'a> {
    format: DocFormat,
    index: &'a SymbolIndex,
    current_module: &'a str,
}

impl RenderCtx<'_> {
    /// Resolve a potential cross-module link target for a type name.
    /// Returns the href (relative URL with anchor) if this name is
    /// defined anywhere in the project.
    fn link_for(&self, type_name: &str) -> Option<String> {
        let module = self.index.module_of(type_name)?;
        let anchor = anchor_for(type_name);
        if module == self.current_module {
            Some(format!("#{anchor}"))
        } else {
            Some(format!("{module}.{}#{anchor}", self.format.extension()))
        }
    }
}

/// GFM-style anchor slug: lowercase, spaces→hyphens, drop punctuation.
fn anchor_for(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("item");
    }
    out
}

// ─── Markdown rendering ───────────────────────────────────────────────────────

fn render_module_markdown(pm: &ParsedModule, _ctx: &RenderCtx<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", pm.name));

    if !pm.module.doc.is_empty() {
        for line in &pm.module.doc {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }

    let buckets = Buckets::from_module(&pm.module);

    if !buckets.fns.is_empty() {
        out.push_str("## Functions\n\n");
        for d in &buckets.fns {
            md_fn(&mut out, d, &pm.source, "###");
        }
    }

    if !buckets.records.is_empty() {
        out.push_str("## Records\n\n");
        for d in &buckets.records {
            md_record(&mut out, d, &pm.source);
        }
    }

    if !buckets.enums.is_empty() {
        out.push_str("## Enums\n\n");
        for d in &buckets.enums {
            md_enum(&mut out, d, &pm.source);
        }
    }

    if !buckets.traits.is_empty() {
        out.push_str("## Traits\n\n");
        for d in &buckets.traits {
            md_trait(&mut out, d, &pm.source);
        }
    }

    if !buckets.effects.is_empty() {
        out.push_str("## Effects\n\n");
        for d in &buckets.effects {
            md_effect(&mut out, d, &pm.source);
        }
    }

    if !buckets.aliases.is_empty() {
        out.push_str("## Type Aliases\n\n");
        for d in &buckets.aliases {
            md_alias(&mut out, d, &pm.source);
        }
    }

    out
}

fn md_fn(out: &mut String, d: &FnDecl, source: &str, heading: &str) {
    let sig = format_fn_signature_plain(d);
    out.push_str(&format!("{heading} `{sig}`\n\n"));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_md(out, &docs);
}

fn md_record(out: &mut String, d: &RecordDecl, source: &str) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_plain(&d.generic_params);
    out.push_str(&format!(
        "### `{vis}record {}{}`\n\n",
        d.name.name, generics
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_md(out, &docs);

    if !d.fields.is_empty() {
        out.push_str("| Field | Type | Default | Description |\n");
        out.push_str("|-------|------|---------|-------------|\n");
        for f in &d.fields {
            let doc = docs_for(source, f.span.start).join(" ");
            let default = f.default.as_ref().map(|_| "(has default)").unwrap_or("");
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                f.name.name,
                format_type_plain(&f.ty),
                default,
                escape_pipes(&doc),
            ));
        }
        out.push('\n');
    }
}

fn md_enum(out: &mut String, d: &EnumDecl, source: &str) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_plain(&d.generic_params);
    out.push_str(&format!("### `{vis}enum {}{}`\n\n", d.name.name, generics));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_md(out, &docs);

    if !d.variants.is_empty() {
        out.push_str("**Variants:**\n\n");
        for v in &d.variants {
            let (name, payload, span_start) = enum_variant_display_plain(v);
            let variant_docs = docs_for(source, span_start).join(" ");
            let suffix = if variant_docs.is_empty() {
                String::new()
            } else {
                format!(" — {variant_docs}")
            };
            out.push_str(&format!("- `{name}{payload}`{suffix}\n"));
        }
        out.push('\n');
    }
}

fn md_trait(out: &mut String, d: &TraitDecl, source: &str) {
    let vis = vis_str(d.visibility);
    let kind = if d.is_platform {
        "platform trait"
    } else {
        "trait"
    };
    let generics = format_generics_plain(&d.generic_params);
    out.push_str(&format!(
        "### `{vis}{kind} {}{}`\n\n",
        d.name.name, generics
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_md(out, &docs);

    if !d.methods.is_empty() {
        out.push_str("**Methods:**\n\n");
        for m in &d.methods {
            md_fn(out, m, source, "####");
        }
    }
}

fn md_effect(out: &mut String, d: &EffectDecl, source: &str) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_plain(&d.generic_params);
    out.push_str(&format!(
        "### `{vis}effect {}{}`\n\n",
        d.name.name, generics
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_md(out, &docs);

    if !d.operations.is_empty() {
        out.push_str("**Operations:**\n\n");
        for op in &d.operations {
            md_fn(out, op, source, "####");
        }
    }
}

fn md_alias(out: &mut String, d: &TypeAliasDecl, source: &str) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_plain(&d.generic_params);
    let ty = format_type_plain(&d.ty);
    out.push_str(&format!(
        "### `{vis}type {}{} = {}`\n\n",
        d.name.name, generics, ty
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_md(out, &docs);
}

fn append_docs_md(out: &mut String, docs: &[String]) {
    if docs.is_empty() {
        return;
    }
    for line in docs {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
}

fn render_markdown_index(modules: &[ParsedModule], project: Option<&ProjectMeta>) -> String {
    let mut out = String::new();
    let (title, version) = project
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .unwrap_or(("API Reference", ""));
    out.push_str(&format!("# {title}\n\n"));
    if !version.is_empty() {
        out.push_str(&format!("**Version:** {version}\n\n"));
    }
    out.push_str("## Modules\n\n");
    for pm in modules {
        let summary = first_doc_line(&pm.module.doc);
        out.push_str(&format!("- [{}]({}.md)", pm.name, pm.slug));
        if !summary.is_empty() {
            out.push_str(&format!(" — {summary}"));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

fn first_doc_line(doc: &[String]) -> String {
    for line in doc {
        let t = line.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    String::new()
}

// ─── HTML rendering ───────────────────────────────────────────────────────────

const HTML_CSS: &str = r#"
body { font-family: system-ui, -apple-system, Segoe UI, sans-serif; margin: 0; color: #222; background: #fff; }
.layout { display: flex; min-height: 100vh; }
.sidebar { width: 240px; background: #f7f7f8; border-right: 1px solid #e4e4e7; padding: 1.25rem 1rem; box-sizing: border-box; position: sticky; top: 0; align-self: flex-start; height: 100vh; overflow-y: auto; }
.sidebar h2 { font-size: 0.85rem; text-transform: uppercase; letter-spacing: 0.05em; color: #666; margin: 0 0 0.5rem 0; }
.sidebar ul { list-style: none; padding: 0; margin: 0 0 1rem 0; }
.sidebar li { margin: 0.15rem 0; }
.sidebar a { display: block; padding: 4px 8px; border-radius: 4px; text-decoration: none; color: #0645ad; font-size: 0.9rem; }
.sidebar a:hover { background: #e8eefc; }
.sidebar a.current { background: #dbe7ff; color: #003e99; font-weight: 600; }
.content { flex: 1; padding: 2rem 2.5rem; max-width: 920px; box-sizing: border-box; }
h1 { margin-top: 0; border-bottom: 1px solid #e4e4e7; padding-bottom: 0.4rem; }
h2 { margin-top: 2rem; border-bottom: 1px solid #f0f0f0; padding-bottom: 0.2rem; }
.item { margin: 1.5rem 0 2rem 0; }
.item h3, .item h4 { margin-bottom: 0.4rem; font-weight: normal; }
.sig { font-family: "SF Mono", SFMono-Regular, Menlo, Consolas, monospace; background: #f4f4f5; padding: 4px 10px; border-radius: 4px; font-size: 0.95rem; display: inline-block; }
.sig .kw { color: #8250df; font-weight: 600; }
.sig .ty { color: #0550ae; }
.sig a.ty { text-decoration: none; border-bottom: 1px dashed #0550ae; }
.sig a.ty:hover { background: #eaf0ff; }
.doc { color: #333; line-height: 1.55; }
table { border-collapse: collapse; margin: 0.5rem 0 1rem 0; font-size: 0.9rem; }
th, td { border: 1px solid #e4e4e7; padding: 6px 10px; text-align: left; vertical-align: top; }
th { background: #fafafa; font-weight: 600; }
td code { font-family: "SF Mono", Menlo, Consolas, monospace; background: #f4f4f5; padding: 1px 4px; border-radius: 3px; font-size: 0.9em; }
ul.variants { list-style: none; padding-left: 0; }
ul.variants li { padding: 3px 0; }
.summary { color: #555; font-size: 0.9rem; margin-left: 0.3rem; }
a { color: #0645ad; }
"#;

fn render_module_html(pm: &ParsedModule, all: &[ParsedModule], ctx: &RenderCtx<'_>) -> String {
    let mut body = String::new();
    body.push_str(&format!("<h1>{}</h1>\n", escape_html(&pm.name)));

    if !pm.module.doc.is_empty() {
        body.push_str("<p class=\"doc\">");
        body.push_str(&escape_html(&pm.module.doc.join(" ")));
        body.push_str("</p>\n");
    }

    let buckets = Buckets::from_module(&pm.module);

    if !buckets.fns.is_empty() {
        body.push_str("<h2>Functions</h2>\n");
        for d in &buckets.fns {
            html_fn(&mut body, d, &pm.source, ctx, "h3");
        }
    }

    if !buckets.records.is_empty() {
        body.push_str("<h2>Records</h2>\n");
        for d in &buckets.records {
            html_record(&mut body, d, &pm.source, ctx);
        }
    }

    if !buckets.enums.is_empty() {
        body.push_str("<h2>Enums</h2>\n");
        for d in &buckets.enums {
            html_enum(&mut body, d, &pm.source, ctx);
        }
    }

    if !buckets.traits.is_empty() {
        body.push_str("<h2>Traits</h2>\n");
        for d in &buckets.traits {
            html_trait(&mut body, d, &pm.source, ctx);
        }
    }

    if !buckets.effects.is_empty() {
        body.push_str("<h2>Effects</h2>\n");
        for d in &buckets.effects {
            html_effect(&mut body, d, &pm.source, ctx);
        }
    }

    if !buckets.aliases.is_empty() {
        body.push_str("<h2>Type Aliases</h2>\n");
        for d in &buckets.aliases {
            html_alias(&mut body, d, &pm.source, ctx);
        }
    }

    wrap_html_page(&pm.name, &body, all, Some(&pm.slug))
}

fn wrap_html_page(
    title: &str,
    body: &str,
    all: &[ParsedModule],
    current_slug: Option<&str>,
) -> String {
    let mut sidebar = String::new();
    sidebar.push_str("<nav class=\"sidebar\">\n");
    sidebar.push_str("<h2>Index</h2>\n<ul>\n");
    let index_current = if current_slug.is_none() {
        " class=\"current\""
    } else {
        ""
    };
    sidebar.push_str(&format!(
        "  <li><a href=\"index.html\"{index_current}>Overview</a></li>\n"
    ));
    sidebar.push_str("</ul>\n");
    sidebar.push_str("<h2>Modules</h2>\n<ul>\n");
    for pm in all {
        let is_current = current_slug == Some(&pm.slug);
        let cur = if is_current { " class=\"current\"" } else { "" };
        sidebar.push_str(&format!(
            "  <li><a href=\"{slug}.html\"{cur}>{name}</a></li>\n",
            slug = escape_html(&pm.slug),
            name = escape_html(&pm.name),
        ));
    }
    sidebar.push_str("</ul>\n</nav>\n");

    format!(
        "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{title} — Bock Docs</title>\n\
<style>{css}</style>\n\
</head>\n\
<body>\n\
<div class=\"layout\">\n\
{sidebar}\
<main class=\"content\">\n\
{body}\
</main>\n\
</div>\n\
</body>\n\
</html>\n",
        title = escape_html(title),
        css = HTML_CSS,
        sidebar = sidebar,
        body = body,
    )
}

fn render_html_index(modules: &[ParsedModule], project: Option<&ProjectMeta>) -> String {
    let (title, version) = project
        .map(|p| (p.name.clone(), p.version.clone()))
        .unwrap_or_else(|| ("API Reference".to_string(), String::new()));

    let mut body = String::new();
    body.push_str(&format!("<h1>{}</h1>\n", escape_html(&title)));
    if !version.is_empty() {
        body.push_str(&format!(
            "<p class=\"doc\"><strong>Version:</strong> {}</p>\n",
            escape_html(&version)
        ));
    }
    body.push_str("<h2>Modules</h2>\n<ul class=\"variants\">\n");
    for pm in modules {
        let summary = first_doc_line(&pm.module.doc);
        body.push_str(&format!(
            "  <li><a href=\"{slug}.html\">{name}</a>",
            slug = escape_html(&pm.slug),
            name = escape_html(&pm.name),
        ));
        if !summary.is_empty() {
            body.push_str(&format!(
                "<span class=\"summary\">— {}</span>",
                escape_html(&summary)
            ));
        }
        body.push_str("</li>\n");
    }
    body.push_str("</ul>\n");

    wrap_html_page(&title, &body, modules, None)
}

fn html_fn(out: &mut String, d: &FnDecl, source: &str, ctx: &RenderCtx<'_>, heading: &str) {
    let sig = format_fn_signature_html(d, ctx);
    let anchor = anchor_for(&d.name.name);
    out.push_str(&format!(
        "<div class=\"item\" id=\"{anchor}\">\n\
<{heading}><span class=\"sig\">{sig}</span></{heading}>\n",
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_html(out, &docs);
    out.push_str("</div>\n");
}

fn html_record(out: &mut String, d: &RecordDecl, source: &str, ctx: &RenderCtx<'_>) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_html(&d.generic_params, ctx);
    let anchor = anchor_for(&d.name.name);
    let vis_html = kw_html_prefix(vis);

    out.push_str(&format!(
        "<div class=\"item\" id=\"{anchor}\">\n<h3><span class=\"sig\">{vis_html}<span class=\"kw\">record</span> <span class=\"ty\">{name}</span>{generics}</span></h3>\n",
        name = escape_html(&d.name.name),
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_html(out, &docs);

    if !d.fields.is_empty() {
        out.push_str("<table>\n<thead><tr><th>Field</th><th>Type</th><th>Default</th><th>Description</th></tr></thead>\n<tbody>\n");
        for f in &d.fields {
            let doc = docs_for(source, f.span.start).join(" ");
            let default = f.default.as_ref().map(|_| "(has default)").unwrap_or("");
            out.push_str(&format!(
                "<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>\n",
                escape_html(&f.name.name),
                format_type_html(&f.ty, ctx),
                escape_html(default),
                escape_html(&doc),
            ));
        }
        out.push_str("</tbody></table>\n");
    }
    out.push_str("</div>\n");
}

fn html_enum(out: &mut String, d: &EnumDecl, source: &str, ctx: &RenderCtx<'_>) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_html(&d.generic_params, ctx);
    let anchor = anchor_for(&d.name.name);
    let vis_html = kw_html_prefix(vis);
    out.push_str(&format!(
        "<div class=\"item\" id=\"{anchor}\">\n<h3><span class=\"sig\">{vis_html}<span class=\"kw\">enum</span> <span class=\"ty\">{name}</span>{generics}</span></h3>\n",
        name = escape_html(&d.name.name),
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_html(out, &docs);

    if !d.variants.is_empty() {
        out.push_str("<p><strong>Variants:</strong></p>\n<ul class=\"variants\">\n");
        for v in &d.variants {
            let (name, payload, span_start) = enum_variant_display_html(v, ctx);
            let variant_docs = docs_for(source, span_start).join(" ");
            let suffix = if variant_docs.is_empty() {
                String::new()
            } else {
                format!(
                    " <span class=\"summary\">— {}</span>",
                    escape_html(&variant_docs)
                )
            };
            out.push_str(&format!(
                "  <li><code>{}{}</code>{}</li>\n",
                escape_html(&name),
                payload,
                suffix,
            ));
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</div>\n");
}

fn html_trait(out: &mut String, d: &TraitDecl, source: &str, ctx: &RenderCtx<'_>) {
    let vis = vis_str(d.visibility);
    let kind = if d.is_platform {
        "platform trait"
    } else {
        "trait"
    };
    let generics = format_generics_html(&d.generic_params, ctx);
    let anchor = anchor_for(&d.name.name);
    let vis_html = kw_html_prefix(vis);
    out.push_str(&format!(
        "<div class=\"item\" id=\"{anchor}\">\n<h3><span class=\"sig\">{vis_html}<span class=\"kw\">{kind}</span> <span class=\"ty\">{name}</span>{generics}</span></h3>\n",
        name = escape_html(&d.name.name),
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_html(out, &docs);

    if !d.methods.is_empty() {
        out.push_str("<p><strong>Methods:</strong></p>\n");
        for m in &d.methods {
            html_fn(out, m, source, ctx, "h4");
        }
    }
    out.push_str("</div>\n");
}

fn html_effect(out: &mut String, d: &EffectDecl, source: &str, ctx: &RenderCtx<'_>) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_html(&d.generic_params, ctx);
    let anchor = anchor_for(&d.name.name);
    let vis_html = kw_html_prefix(vis);
    out.push_str(&format!(
        "<div class=\"item\" id=\"{anchor}\">\n<h3><span class=\"sig\">{vis_html}<span class=\"kw\">effect</span> <span class=\"ty\">{name}</span>{generics}</span></h3>\n",
        name = escape_html(&d.name.name),
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_html(out, &docs);

    if !d.operations.is_empty() {
        out.push_str("<p><strong>Operations:</strong></p>\n");
        for op in &d.operations {
            html_fn(out, op, source, ctx, "h4");
        }
    }
    out.push_str("</div>\n");
}

fn html_alias(out: &mut String, d: &TypeAliasDecl, source: &str, ctx: &RenderCtx<'_>) {
    let vis = vis_str(d.visibility);
    let generics = format_generics_html(&d.generic_params, ctx);
    let ty = format_type_html(&d.ty, ctx);
    let anchor = anchor_for(&d.name.name);
    let vis_html = kw_html_prefix(vis);
    out.push_str(&format!(
        "<div class=\"item\" id=\"{anchor}\">\n<h3><span class=\"sig\">{vis_html}<span class=\"kw\">type</span> <span class=\"ty\">{name}</span>{generics} = {ty}</span></h3>\n",
        name = escape_html(&d.name.name),
    ));
    let docs = docs_for(source, declaration_start(d.span.start, &d.annotations));
    append_docs_html(out, &docs);
    out.push_str("</div>\n");
}

fn append_docs_html(out: &mut String, docs: &[String]) {
    if docs.is_empty() {
        return;
    }
    let joined = docs.join(" ");
    out.push_str("<p class=\"doc\">");
    out.push_str(&escape_html(&joined));
    out.push_str("</p>\n");
}

fn kw_html_prefix(vis: &'static str) -> String {
    if vis.is_empty() {
        String::new()
    } else {
        format!("<span class=\"kw\">{}</span> ", escape_html(vis.trim_end()))
    }
}

fn format_fn_signature_html(d: &FnDecl, ctx: &RenderCtx<'_>) -> String {
    let mut s = String::new();
    let vis = vis_str(d.visibility);
    if !vis.is_empty() {
        s.push_str(&format!(
            "<span class=\"kw\">{}</span> ",
            escape_html(vis.trim_end())
        ));
    }
    if d.is_async {
        s.push_str("<span class=\"kw\">async</span> ");
    }
    s.push_str("<span class=\"kw\">fn</span> ");
    s.push_str(&format!(
        "<span class=\"ty\">{}</span>",
        escape_html(&d.name.name)
    ));
    s.push_str(&format_generics_html(&d.generic_params, ctx));
    s.push('(');
    let parts: Vec<String> = d.params.iter().map(|p| format_param_html(p, ctx)).collect();
    s.push_str(&parts.join(", "));
    s.push(')');
    if let Some(ret) = &d.return_type {
        s.push_str(" -&gt; ");
        s.push_str(&format_type_html(ret, ctx));
    }
    if !d.effect_clause.is_empty() {
        s.push_str(" <span class=\"kw\">with</span> ");
        let e: Vec<String> = d
            .effect_clause
            .iter()
            .map(|p| format_type_path_html(p, ctx))
            .collect();
        s.push_str(&e.join(", "));
    }
    s
}

fn format_param_html(p: &Param, ctx: &RenderCtx<'_>) -> String {
    let name = escape_html(&pattern_name(&p.pattern));
    match &p.ty {
        Some(t) => format!("{}: {}", name, format_type_html(t, ctx)),
        None => name,
    }
}

fn format_type_html(t: &TypeExpr, ctx: &RenderCtx<'_>) -> String {
    match t {
        TypeExpr::Named { path, args, .. } => {
            let base = format_type_path_html(path, ctx);
            if args.is_empty() {
                base
            } else {
                let arg_strs: Vec<String> = args.iter().map(|a| format_type_html(a, ctx)).collect();
                format!("{}[{}]", base, arg_strs.join(", "))
            }
        }
        TypeExpr::Tuple { elems, .. } => {
            let parts: Vec<String> = elems.iter().map(|a| format_type_html(a, ctx)).collect();
            format!("({})", parts.join(", "))
        }
        TypeExpr::Function {
            params,
            ret,
            effects,
            ..
        } => {
            let ps: Vec<String> = params.iter().map(|a| format_type_html(a, ctx)).collect();
            let eff = if effects.is_empty() {
                String::new()
            } else {
                let e: Vec<String> = effects
                    .iter()
                    .map(|p| format_type_path_html(p, ctx))
                    .collect();
                format!(" <span class=\"kw\">with</span> {}", e.join(", "))
            };
            format!(
                "<span class=\"ty\">Fn</span>({}) -&gt; {}{}",
                ps.join(", "),
                format_type_html(ret, ctx),
                eff,
            )
        }
        TypeExpr::Optional { inner, .. } => format!("{}?", format_type_html(inner, ctx)),
        TypeExpr::SelfType { .. } => "<span class=\"kw\">Self</span>".to_string(),
    }
}

fn format_type_path_html(p: &TypePath, ctx: &RenderCtx<'_>) -> String {
    // Link on the first segment if we know a module that defines it.
    let segments: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
    let joined = segments.join(".");
    let primary = segments.first().copied().unwrap_or("");
    if let Some(href) = ctx.link_for(primary) {
        format!(
            "<a class=\"ty\" href=\"{}\">{}</a>",
            escape_html(&href),
            escape_html(&joined)
        )
    } else {
        format!("<span class=\"ty\">{}</span>", escape_html(&joined))
    }
}

fn format_generics_html(params: &[GenericParam], ctx: &RenderCtx<'_>) -> String {
    if params.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = params
        .iter()
        .map(|p| {
            if p.bounds.is_empty() {
                escape_html(&p.name.name)
            } else {
                let b: Vec<String> = p
                    .bounds
                    .iter()
                    .map(|bp| format_type_path_html(bp, ctx))
                    .collect();
                format!("{}: {}", escape_html(&p.name.name), b.join(" + "))
            }
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn enum_variant_display_html(v: &EnumVariant, ctx: &RenderCtx<'_>) -> (String, String, usize) {
    match v {
        EnumVariant::Unit { name, span, .. } => (name.name.clone(), String::new(), span.start),
        EnumVariant::Tuple {
            name, tys, span, ..
        } => {
            let ts: Vec<String> = tys.iter().map(|t| format_type_html(t, ctx)).collect();
            (
                name.name.clone(),
                format!("({})", ts.join(", ")),
                span.start,
            )
        }
        EnumVariant::Struct {
            name, fields, span, ..
        } => {
            let fs: Vec<String> = fields
                .iter()
                .map(|f: &RecordDeclField| {
                    format!(
                        "{}: {}",
                        escape_html(&f.name.name),
                        format_type_html(&f.ty, ctx)
                    )
                })
                .collect();
            (
                name.name.clone(),
                format!(" {{ {} }}", fs.join(", ")),
                span.start,
            )
        }
    }
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

struct Buckets<'a> {
    fns: Vec<&'a FnDecl>,
    records: Vec<&'a RecordDecl>,
    enums: Vec<&'a EnumDecl>,
    traits: Vec<&'a TraitDecl>,
    effects: Vec<&'a EffectDecl>,
    aliases: Vec<&'a TypeAliasDecl>,
}

impl<'a> Buckets<'a> {
    fn from_module(module: &'a Module) -> Self {
        let mut b = Buckets {
            fns: Vec::new(),
            records: Vec::new(),
            enums: Vec::new(),
            traits: Vec::new(),
            effects: Vec::new(),
            aliases: Vec::new(),
        };
        for item in &module.items {
            match item {
                Item::Fn(d) => b.fns.push(d),
                Item::Record(d) => b.records.push(d),
                Item::Enum(d) => b.enums.push(d),
                Item::Trait(d) | Item::PlatformTrait(d) => b.traits.push(d),
                Item::Effect(d) => b.effects.push(d),
                Item::TypeAlias(d) => b.aliases.push(d),
                _ => {}
            }
        }
        b
    }
}

fn vis_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public ",
        Visibility::Internal => "internal ",
        Visibility::Private => "",
    }
}

fn format_generics_plain(params: &[GenericParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = params
        .iter()
        .map(|p| {
            if p.bounds.is_empty() {
                p.name.name.clone()
            } else {
                let b: Vec<String> = p.bounds.iter().map(format_type_path_plain).collect();
                format!("{}: {}", p.name.name, b.join(" + "))
            }
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn format_fn_signature_plain(d: &FnDecl) -> String {
    let vis = vis_str(d.visibility);
    let asyn = if d.is_async { "async " } else { "" };
    let generics = format_generics_plain(&d.generic_params);
    let params: Vec<String> = d.params.iter().map(format_param_plain).collect();
    let ret = d
        .return_type
        .as_ref()
        .map(|t| format!(" -> {}", format_type_plain(t)))
        .unwrap_or_default();
    let effects = if d.effect_clause.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = d.effect_clause.iter().map(format_type_path_plain).collect();
        format!(" with {}", names.join(", "))
    };
    format!(
        "{vis}{asyn}fn {}{}({}){}{}",
        d.name.name,
        generics,
        params.join(", "),
        ret,
        effects,
    )
}

fn format_param_plain(p: &Param) -> String {
    let name = pattern_name(&p.pattern);
    match &p.ty {
        Some(t) => format!("{}: {}", name, format_type_plain(t)),
        None => name,
    }
}

fn pattern_name(p: &bock_ast::Pattern) -> String {
    match p {
        bock_ast::Pattern::Bind { name, .. } | bock_ast::Pattern::MutBind { name, .. } => {
            name.name.clone()
        }
        bock_ast::Pattern::Wildcard { .. } => "_".to_string(),
        _ => "<pat>".to_string(),
    }
}

fn format_type_plain(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named { path, args, .. } => {
            let base = format_type_path_plain(path);
            if args.is_empty() {
                base
            } else {
                let arg_strs: Vec<String> = args.iter().map(format_type_plain).collect();
                format!("{}[{}]", base, arg_strs.join(", "))
            }
        }
        TypeExpr::Tuple { elems, .. } => {
            let parts: Vec<String> = elems.iter().map(format_type_plain).collect();
            format!("({})", parts.join(", "))
        }
        TypeExpr::Function {
            params,
            ret,
            effects,
            ..
        } => {
            let ps: Vec<String> = params.iter().map(format_type_plain).collect();
            let eff = if effects.is_empty() {
                String::new()
            } else {
                let e: Vec<String> = effects.iter().map(format_type_path_plain).collect();
                format!(" with {}", e.join(", "))
            };
            format!("Fn({}) -> {}{}", ps.join(", "), format_type_plain(ret), eff)
        }
        TypeExpr::Optional { inner, .. } => format!("{}?", format_type_plain(inner)),
        TypeExpr::SelfType { .. } => "Self".to_string(),
    }
}

fn format_type_path_plain(p: &TypePath) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

fn enum_variant_display_plain(v: &EnumVariant) -> (String, String, usize) {
    match v {
        EnumVariant::Unit { name, span, .. } => (name.name.clone(), String::new(), span.start),
        EnumVariant::Tuple {
            name, tys, span, ..
        } => {
            let ts: Vec<String> = tys.iter().map(format_type_plain).collect();
            (
                name.name.clone(),
                format!("({})", ts.join(", ")),
                span.start,
            )
        }
        EnumVariant::Struct {
            name, fields, span, ..
        } => {
            let fs: Vec<String> = fields
                .iter()
                .map(|f: &RecordDeclField| format!("{}: {}", f.name.name, format_type_plain(&f.ty)))
                .collect();
            (
                name.name.clone(),
                format!(" {{ {} }}", fs.join(", ")),
                span.start,
            )
        }
    }
}

fn escape_pipes(s: &str) -> String {
    s.replace('|', "\\|")
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Compute the effective byte start of a declaration, including its
/// leading annotations. Doc comments appear *before* this position.
fn declaration_start(item_span_start: usize, annotations: &[Annotation]) -> usize {
    annotations
        .iter()
        .map(|a| a.span.start)
        .min()
        .unwrap_or(item_span_start)
        .min(item_span_start)
}

/// Collect `///` doc comment lines immediately preceding the byte offset
/// `item_start` in the source.
fn docs_for(source: &str, item_start: usize) -> Vec<String> {
    let mut docs = Vec::new();
    let mut cursor = source[..item_start].rfind('\n').map(|i| i + 1).unwrap_or(0);

    loop {
        if cursor == 0 {
            break;
        }
        let prev_end = cursor - 1;
        let prev_start = source[..prev_end].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = &source[prev_start..prev_end];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            break;
        } else if let Some(rest) = trimmed.strip_prefix("///") {
            if rest.starts_with('/') {
                break;
            }
            docs.push(rest.trim().to_string());
            cursor = prev_start;
        } else if trimmed.starts_with('@') {
            cursor = prev_start;
        } else {
            break;
        }
    }

    docs.reverse();
    docs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_for_simple_function() {
        let src = "/// first line\n/// second line\nfn foo() {}\n";
        let item_start = src.find("fn foo").unwrap();
        let docs = docs_for(src, item_start);
        assert_eq!(docs, vec!["first line", "second line"]);
    }

    #[test]
    fn docs_for_skips_annotations() {
        let src = "/// the doc\n@derive(Equatable)\nrecord Point { x: Int }\n";
        let item_start = src.find("@derive").unwrap();
        let docs = docs_for(src, item_start);
        assert_eq!(docs, vec!["the doc"]);
    }

    #[test]
    fn docs_for_breaks_on_blank_line() {
        let src = "/// orphan\n\nfn foo() {}\n";
        let item_start = src.find("fn foo").unwrap();
        let docs = docs_for(src, item_start);
        assert!(docs.is_empty());
    }

    #[test]
    fn docs_for_no_docs() {
        let src = "fn foo() {}\n";
        let docs = docs_for(src, 0);
        assert!(docs.is_empty());
    }

    #[test]
    fn docs_for_preserves_blank_doc_line() {
        let src = "/// first\n///\n/// third\nfn foo() {}\n";
        let item_start = src.find("fn foo").unwrap();
        let docs = docs_for(src, item_start);
        assert_eq!(docs, vec!["first", "", "third"]);
    }

    #[test]
    fn anchor_for_slugifies() {
        assert_eq!(anchor_for("Foo"), "foo");
        assert_eq!(anchor_for("Foo Bar"), "foo-bar");
        assert_eq!(anchor_for("Foo_Bar"), "foo_bar");
        assert_eq!(anchor_for("!!!"), "item");
    }

    #[test]
    fn docformat_parse() {
        assert_eq!(DocFormat::parse("md").unwrap(), DocFormat::Markdown);
        assert_eq!(DocFormat::parse("markdown").unwrap(), DocFormat::Markdown);
        assert_eq!(DocFormat::parse("HTML").unwrap(), DocFormat::Html);
        assert!(DocFormat::parse("pdf").is_err());
    }

    #[test]
    fn escape_html_basic() {
        assert_eq!(escape_html("a<b>&c"), "a&lt;b&gt;&amp;c");
    }

    #[test]
    fn strip_field_quoted() {
        assert_eq!(
            strip_field("name = \"foo\"", "name"),
            Some("foo".to_string())
        );
        assert_eq!(
            strip_field("version=\"0.1.0\"", "version"),
            Some("0.1.0".to_string())
        );
        assert_eq!(
            strip_field("version = 0.1", "version"),
            Some("0.1".to_string())
        );
        assert_eq!(strip_field("other = 1", "name"), None);
    }
}
