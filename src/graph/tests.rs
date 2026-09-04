use super::*;

const SAMPLE: &str = r#"pub struct Point {
    x: f64,
    y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

pub fn add(a: f64, b: f64) -> f64 {
    helper();
    helper();
    let p = Point::new(a, b);
    p.norm() + a + b
}

fn helper() {
    mystery();
    println!("hi");
}

pub const LIMIT: u32 = 42;

pub enum Shape {
    Circle(f64),
}

pub type Alias = f64;
"#;

fn build(sample: &str) -> CodeGraph {
    let mut g = CodeGraph::default();
    g.add_source("lib.rs", sample);
    g.finish();
    g
}

#[test]
fn extracts_top_level_symbols() {
    let g = build(SAMPLE);
    let names: Vec<(&str, &str)> = g.nodes.iter().map(|s| (s.kind.as_str(), s.name.as_str())).collect();
    assert!(names.contains(&("fn", "add")), "missing fn add: {names:?}");
    assert!(names.contains(&("fn", "helper")), "missing fn helper: {names:?}");
    assert!(names.contains(&("struct", "Point")), "missing struct Point: {names:?}");
    assert!(names.contains(&("enum", "Shape")), "missing enum Shape: {names:?}");
    assert!(names.contains(&("type", "Alias")), "missing type Alias: {names:?}");
    assert!(names.contains(&("const", "LIMIT")), "missing const LIMIT: {names:?}");
}

#[test]
fn qualifies_impl_methods() {
    let g = build(SAMPLE);
    let norm = g.nodes.iter().find(|s| s.name == "Point::norm").expect("Point::norm node");
    assert_eq!(norm.kind, "method");
    assert_eq!(norm.file, "lib.rs");
    assert!(norm.line > 0);
    assert!(g.nodes.iter().any(|s| s.name == "Point::new" && s.kind == "method"));
}

#[test]
fn edges_bare_call() {
    let g = build(SAMPLE);
    assert!(
        g.edges.iter().any(|e| e.from == "add" && e.to == "helper"),
        "missing add -> helper: {:?}",
        g.edges
    );
}

#[test]
fn edges_scoped_call_uses_hint() {
    let g = build(SAMPLE);
    assert!(
        g.edges.iter().any(|e| e.from == "add" && e.to == "Point::new"),
        "missing add -> Point::new: {:?}",
        g.edges
    );
}

#[test]
fn edges_receiver_method_call() {
    let g = build(SAMPLE);
    assert!(
        g.edges.iter().any(|e| e.from == "add" && e.to == "Point::norm"),
        "missing add -> Point::norm: {:?}",
        g.edges
    );
}

#[test]
fn unresolved_calls_excluded_and_counted() {
    let g = build(SAMPLE);
    // mystery() is undefined; println! is a macro (not a call_expression).
    assert!(!g.edges.iter().any(|e| e.to.contains("mystery")), "mystery must not resolve");
    assert!(g.unresolved_calls >= 1, "expected >=1 unresolved, got {}", g.unresolved_calls);
}

#[test]
fn duplicate_call_sites_dedupe_to_one_edge() {
    let g = build(SAMPLE);
    let n = g.edges.iter().filter(|e| e.from == "add" && e.to == "helper").count();
    assert_eq!(n, 1, "add -> helper should dedupe, got {n}");
}

#[test]
fn unparsed_file_reported_not_fatal() {
    let mut g = CodeGraph::default();
    g.add_source("bad.rs", "fn ( this is not rust }{");
    g.finish();
    assert_eq!(g.files_scanned, 1);
    assert_eq!(g.files_parsed, 0);
    assert_eq!(g.unparsed, vec!["bad.rs".to_string()]);
    assert!(g.nodes.is_empty());
}

#[test]
fn json_output_parses() {
    let g = build(SAMPLE);
    let v: serde_json::Value =
        serde_json::from_str(&render_json(&g)).expect("json must parse");
    assert!(v["nodes"].is_array());
    assert!(v["edges"].is_array());
    assert!(v["unresolved_calls"].is_number());
}

#[test]
fn output_is_deterministic() {
    let a = render_text(&build(SAMPLE));
    let b = render_text(&build(SAMPLE));
    assert_eq!(a, b);
}

#[test]
fn graph_smaller_than_input() {
    let g = build(SAMPLE);
    let t = render_text(&g);
    assert!(t.len() < SAMPLE.len(), "graph {} >= input {}", t.len(), SAMPLE.len());
}

#[test]
fn cross_file_resolution() {
    let mut g = CodeGraph::default();
    g.add_source("a.rs", "fn caller() { callee(); }\n");
    g.add_source("b.rs", "fn callee() {}\n");
    g.finish();
    assert!(
        g.edges.iter().any(|e| e.from == "caller" && e.to == "callee" && e.file == "a.rs"),
        "missing cross-file edge: {:?}",
        g.edges
    );
}

#[test]
fn collect_rust_files_walks_and_skips() {
    let root = std::env::temp_dir().join(format!("ctx-prune-graph-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
    std::fs::write(root.join("sub").join("b.rs"), "fn b() {}").unwrap();
    std::fs::write(root.join("target").join("c.rs"), "fn c() {}").unwrap();
    std::fs::write(root.join(".git").join("d.rs"), "fn d() {}").unwrap();
    std::fs::write(root.join("notes.txt"), "not rust").unwrap();

    let files = collect_rust_files(&root).expect("walk works");
    let names: Vec<String> = files
        .iter()
        .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(names, vec!["a.rs".to_string(), "sub/b.rs".to_string()], "got {names:?}");

    std::fs::remove_dir_all(&root).ok();
}
