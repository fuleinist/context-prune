//! Tree-sitter-aware code skeletonization (SPEC v2 stretch item:
//! "Tree-sitter-aware code summarization (keep signatures, drop bodies)").
//!
//! Parses Rust source, keeps top-level structure and signatures, replaces
//! bodies/field lists/variant lists with `/* …N lines elided… */` markers.
//! F5 safety: any parse error returns None so callers pass input through.

#[cfg(test)]
mod tests;

use tree_sitter::{Node, Parser};

/// Node kinds whose full text is replaced by an elision marker.
const ELIDABLE: &[&str] = &[
    "block",
    "field_declaration_list",
    "enum_variant_list",
];

// Note: "declaration_list" is recursed into (nested fn signatures survive),
// not elided wholesale.

/// Produce a skeleton of Rust `source`. Returns None when the input does
/// not parse cleanly (callers must then forward the input unchanged).
pub fn rust_skeleton(source: &str) -> Option<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut out = String::with_capacity(source.len() / 2);
    render(source, root, &mut out);
    Some(out)
}

fn render(source: &str, node: Node, out: &mut String) {
    let kind = node.kind();

    if kind == "line_comment" {
        // Keep doc comments only; ordinary comments are noise.
        let t = &source[node.byte_range()];
        if t.starts_with("///") || t.starts_with("//!") {
            out.push_str(t);
        }
        return;
    }

    if ELIDABLE.contains(&kind) {
        let n = node
            .end_position()
            .row
            .saturating_sub(node.start_position().row)
            + 1;
        out.push_str(&format!(" /* …{n} lines elided… */ "));
        return;
    }

    if node.child_count() == 0 {
        out.push_str(&source[node.byte_range()]);
        return;
    }

    let mut cursor = node.walk();
    let mut last_end = node.start_byte();
    for child in node.children(&mut cursor) {
        if child.start_byte() > last_end {
            out.push_str(&source[last_end..child.start_byte()]);
        }
        render(source, child, out);
        last_end = child.end_byte();
    }
    if node.end_byte() > last_end {
        out.push_str(&source[last_end..node.end_byte()]);
    }
}
