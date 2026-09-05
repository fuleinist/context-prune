//! Local code-graph context builder (SPEC v2 stretch goal, cycle 11).
//!
//! Builds a compact symbol + call graph for Rust code: the structural map a
//! model usually needs instead of whole files. Nodes are top-level items and
//! impl methods; edges are call sites resolved BY NAME against the defined
//! symbol set (bare `foo()`, `Path::seg()`, `recv.method()` — last-segment
//! match). Deterministic: sorted nodes/edges, deduped edges.
//!
//! v1 limits (documented honestly): no type-resolved resolution — a bare
//! name shared by two methods is ambiguous and counts as unresolved; calls
//! inside nested fns attribute to the nearest graph-tracked function; trait
//! default methods are not collected as symbols.
//!
//! F5 safety: unparseable files are listed in `unparsed`, never fatal.

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

/// Directory names never descended into during scans.
const SKIP_DIRS: &[&str] = &["target", "node_modules"];

/// One graph node: a symbol defined in the scanned code.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Symbol {
    /// Qualified name: `foo` for free fns, `Point::norm` for impl methods.
    pub name: String,
    /// One of: fn, method, struct, enum, trait, union, type, const, static.
    pub kind: String,
    /// Source file (relative, `/`-separated).
    pub file: String,
    /// 1-based line number.
    pub line: usize,
}

/// One resolved call edge, deduped per (from, to).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// File + line of the first call site seen for this pair.
    pub file: String,
    pub line: usize,
}

/// A call site awaiting resolution (internal).
#[derive(Debug)]
struct CallSite {
    caller: String,
    /// Last-segment callee name (the only part matched in v1).
    name: String,
    /// `Foo::bar` when the site is `Foo::bar(...)` with a single-segment
    /// path — tried against qualified names before the bare-name fallback.
    hint: Option<String>,
    file: String,
    line: usize,
}

/// The accumulated graph across all scanned files.
#[derive(Debug, Default, serde::Serialize)]
pub struct CodeGraph {
    pub files_scanned: usize,
    pub files_parsed: usize,
    pub input_bytes: usize,
    /// Call sites whose callee could not be resolved (ambiguous or unknown).
    pub unresolved_calls: usize,
    /// Files that did not parse as Rust (skipped, never fatal).
    pub unparsed: Vec<String>,
    pub nodes: Vec<Symbol>,
    pub edges: Vec<Edge>,
    #[serde(skip)]
    call_sites: Vec<CallSite>,
    #[serde(skip)]
    callables: Vec<Symbol>,
}

impl CodeGraph {
    /// Add one source file to the graph. `file` is the display path.
    pub fn add_source(&mut self, file: &str, source: &str) {
        self.files_scanned += 1;
        self.input_bytes += source.len();
        let Some(tree) = parse(source) else {
            self.unparsed.push(file.to_string());
            return;
        };
        self.files_parsed += 1;
        let root = tree.root_node();
        collect_items(root, source, file, &mut self.nodes, &mut self.callables);
        collect_call_sites(root, source, file, None, None, &mut self.call_sites);
    }

    /// Resolve call sites into edges and sort everything deterministically.
    pub fn finish(&mut self) {
        let qualified: BTreeSet<String> =
            self.callables.iter().map(|s| s.name.clone()).collect();
        let mut by_bare: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for s in &self.callables {
            let bare = s.name.rsplit("::").next().unwrap_or(&s.name);
            by_bare.entry(bare.to_string()).or_default().push(s.name.clone());
        }

        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        for site in &self.call_sites {
            // Edges from callers that are not graph symbols (e.g. calls
            // attributed to a trait default method we don't track) are
            // dropped rather than leaving dangling `from` names.
            if !qualified.contains(&site.caller) {
                continue;
            }
            let mut resolved: Option<String> = None;
            if let Some(hint) = &site.hint {
                if qualified.contains(hint) {
                    resolved = Some(hint.clone());
                }
            }
            if resolved.is_none() {
                if let Some(cands) = by_bare.get(&site.name) {
                    if cands.len() == 1 {
                        resolved = Some(cands[0].clone());
                    }
                }
            }
            match resolved {
                Some(to) => {
                    if seen.insert((site.caller.clone(), to.clone())) {
                        self.edges.push(Edge {
                            from: site.caller.clone(),
                            to,
                            file: site.file.clone(),
                            line: site.line,
                        });
                    }
                }
                None => self.unresolved_calls += 1,
            }
        }

        self.nodes
            .sort_by(|a, b| (&a.file, a.line, &a.name).cmp(&(&b.file, b.line, &b.name)));
        self.edges.sort_by(|a, b| {
            (&a.from, &a.to, &a.file, a.line).cmp(&(&b.from, &b.to, &b.file, b.line))
        });
    }
}

/// Render the graph as compact text (nodes, then edges).
pub fn render_text(g: &CodeGraph) -> String {
    let mut out = String::new();
    out.push_str("nodes:\n");
    for s in &g.nodes {
        out.push_str(&format!("{} {}\t{}:{}\n", s.kind, s.name, s.file, s.line));
    }
    out.push_str("edges:\n");
    for e in &g.edges {
        out.push_str(&format!("{} -> {}\t{}:{}\n", e.from, e.to, e.file, e.line));
    }
    out
}

/// Render the graph as JSON (nodes, edges, stats).
pub fn render_json(g: &CodeGraph) -> String {
    serde_json::to_string_pretty(g).unwrap_or_else(|_| "{}".to_string())
}

/// Collect `.rs` files: `root` as-is when it is a file, else recursive walk.
/// Skips hidden dirs, `target`, `node_modules`. Sorted for determinism.
pub fn collect_rust_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if root.is_file() {
        if root.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(root.to_path_buf());
        }
        return Ok(out);
    }
    walk_dir(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_dir(&path, out)?;
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn parse(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    Some(tree)
}

fn text(node: Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

fn field_text(node: Node, field: &str, source: &str) -> Option<String> {
    Some(text(node.child_by_field_name(field)?, source))
}

/// Collect symbol nodes from the direct children of a declaration list
/// (source_file, mod body). Impl methods are qualified as `Type::method`.
fn collect_items(
    node: Node,
    source: &str,
    file: &str,
    nodes: &mut Vec<Symbol>,
    callables: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let line = child.start_position().row + 1;
        match child.kind() {
            "function_item" => {
                if let Some(name) = field_text(child, "name", source) {
                    let sym = Symbol { name, kind: "fn".into(), file: file.into(), line };
                    callables.push(sym.clone());
                    nodes.push(sym);
                }
            }
            "struct_item" | "enum_item" | "trait_item" | "union_item" => {
                let kind = match child.kind() {
                    "struct_item" => "struct",
                    "enum_item" => "enum",
                    "trait_item" => "trait",
                    _ => "union",
                };
                if let Some(name) = field_text(child, "name", source) {
                    nodes.push(Symbol { name, kind: kind.into(), file: file.into(), line });
                }
            }
            "type_item" => {
                if let Some(name) = field_text(child, "name", source) {
                    nodes.push(Symbol { name, kind: "type".into(), file: file.into(), line });
                }
            }
            "const_item" | "static_item" => {
                let kind = if child.kind() == "const_item" { "const" } else { "static" };
                if let Some(name) = field_text(child, "name", source) {
                    nodes.push(Symbol { name, kind: kind.into(), file: file.into(), line });
                }
            }
            "impl_item" => {
                let ty = impl_type_name(child, source).unwrap_or_else(|| "?".to_string());
                if let Some(body) = child.child_by_field_name("body") {
                    let mut bc = body.walk();
                    for m in body.children(&mut bc) {
                        if m.kind() == "function_item" {
                            if let Some(mn) = field_text(m, "name", source) {
                                let sym = Symbol {
                                    name: format!("{ty}::{mn}"),
                                    kind: "method".into(),
                                    file: file.into(),
                                    line: m.start_position().row + 1,
                                };
                                callables.push(sym.clone());
                                nodes.push(sym);
                            }
                        }
                    }
                }
            }
            "mod_item" => {
                // Inline modules: items inside are collected unqualified (v1).
                if let Some(body) = child.child_by_field_name("body") {
                    collect_items(body, source, file, nodes, callables);
                }
            }
            _ => {}
        }
    }
}

/// The self-type name of an impl block (generics stripped).
fn impl_type_name(impl_node: Node, source: &str) -> Option<String> {
    let ty = impl_node.child_by_field_name("type")?;
    Some(match ty.kind() {
        "generic_type" => ty
            .child_by_field_name("type")
            .map(|t| text(t, source))
            .unwrap_or_else(|| text(ty, source)),
        _ => text(ty, source),
    })
}

/// Walk the whole file collecting call sites. `caller` is the nearest
/// graph-tracked scope (top-level fn, impl method, const/static); nested
/// fns inherit it. `impl_ctx` qualifies methods found while caller is unset.
fn collect_call_sites(
    node: Node,
    source: &str,
    file: &str,
    impl_ctx: Option<&str>,
    caller: Option<&str>,
    sites: &mut Vec<CallSite>,
) {
    let kind = node.kind();
    // Owned storage so borrows outlive this frame into the recursion.
    let impl_owned: Option<String> = if kind == "impl_item" {
        Some(impl_type_name(node, source).unwrap_or_else(|| "?".to_string()))
    } else {
        None
    };
    let caller_owned: Option<String> = if caller.is_none() {
        match kind {
            "function_item" => {
                field_text(node, "name", source).map(|name| match impl_ctx {
                    Some(ctx) => format!("{ctx}::{name}"),
                    None => name,
                })
            }
            "const_item" | "static_item" => field_text(node, "name", source),
            _ => None,
        }
    } else {
        None
    };
    let (impl_next, caller_next): (Option<&str>, Option<&str>) = match kind {
        "impl_item" => (impl_owned.as_deref(), caller),
        "mod_item" => (None, caller),
        _ => (impl_ctx, caller_owned.as_deref().or(caller)),
    };

    if kind == "call_expression" {
        if let Some(c) = caller {
            if let Some((name, hint)) = callee_name(node, source) {
                sites.push(CallSite {
                    caller: c.to_string(),
                    name,
                    hint,
                    file: file.to_string(),
                    line: node.start_position().row + 1,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_sites(child, source, file, impl_next, caller_next, sites);
    }
}

/// Callee name from a call_expression: (last-segment name, optional
/// `Foo::bar` hint for single-segment scoped paths).
fn callee_name(call: Node, source: &str) -> Option<(String, Option<String>)> {
    let mut f = call.child_by_field_name("function")?;
    if f.kind() == "generic_function" {
        f = f.child_by_field_name("function")?;
    }
    match f.kind() {
        "identifier" => Some((text(f, source), None)),
        "field_expression" => {
            let fld = f.child_by_field_name("field")?;
            Some((text(fld, source), None))
        }
        "scoped_identifier" => {
            let name_node = f.child_by_field_name("name")?;
            let name = text(name_node, source);
            let hint = f.child_by_field_name("path").and_then(|p| {
                if p.kind() == "identifier" {
                    Some(format!("{}::{}", text(p, source), name))
                } else {
                    None
                }
            });
            Some((name, hint))
        }
        _ => None,
    }
}
