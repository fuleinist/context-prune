use super::*;

const SAMPLE: &str = r#"use std::io;

/// Adds two numbers.
pub fn add(a: u32, b: u32) -> u32 {
    let sum = a + b;
    sum
}

struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

pub const LIMIT: u32 = 42;

fn helper() {
    for i in 0..10 {
        println!("{i}");
    }
}
"#;

#[test]
fn keeps_signatures_drops_bodies() {
    let sk = rust_skeleton(SAMPLE).expect("sample should parse");
    // signatures survive
    assert!(sk.contains("pub fn add(a: u32, b: u32) -> u32"), "add sig missing:\n{sk}");
    assert!(sk.contains("fn norm(&self) -> f64"), "norm sig missing:\n{sk}");
    assert!(sk.contains("fn helper()"), "helper sig missing:\n{sk}");
    assert!(sk.contains("struct Point"), "struct header missing:\n{sk}");
    assert!(sk.contains("impl Point"), "impl header missing:\n{sk}");
    assert!(sk.contains("pub const LIMIT: u32 = 42;"), "const missing:\n{sk}");
    assert!(sk.contains("use std::io;"), "use missing:\n{sk}");
    assert!(sk.contains("/// Adds two numbers."), "doc comment missing:\n{sk}");
    // bodies dropped
    assert!(!sk.contains("let sum"), "body leaked:\n{sk}");
    assert!(!sk.contains("println!"), "body leaked:\n{sk}");
    assert!(!sk.contains(".sqrt()"), "body leaked:\n{sk}");
    assert!(!sk.contains("x: f64"), "field list leaked:\n{sk}");
    // markers carry line counts
    assert!(sk.contains("lines elided"), "elision marker missing:\n{sk}");
}

#[test]
fn smaller_than_input() {
    let sk = rust_skeleton(SAMPLE).unwrap();
    assert!(sk.len() < SAMPLE.len(), "skeleton {} >= input {}", sk.len(), SAMPLE.len());
}

#[test]
fn elision_marker_carries_line_count() {
    let sk = rust_skeleton(SAMPLE).unwrap();
    // helper() body spans 5 lines in SAMPLE
    assert!(sk.contains("5 lines elided"), "expected 5-line marker:\n{sk}");
}

#[test]
fn unparseable_returns_none() {
    assert!(rust_skeleton("fn ( this is not rust }{").is_none());
}

#[test]
fn empty_input_parses_to_empty() {
    let sk = rust_skeleton("").expect("empty parses");
    assert_eq!(sk, "");
}
