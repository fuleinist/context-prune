//! Deterministic, meaning-preserving text compression.
//!
//! Every transform drops formatting noise, never facts. Any transform that
//! errors or looks risky is skipped — the caller always gets *some* valid
//! string back, worst case the original.

/// Result of compressing one blob.
#[derive(Debug, Clone, Default)]
pub struct CompressOutcome {
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub transforms_applied: Vec<&'static str>,
}

impl CompressOutcome {
    /// 0.0 = no savings, 1.0 = everything removed.
    pub fn ratio(&self) -> f64 {
        if self.input_bytes == 0 {
            return 0.0;
        }
        1.0 - (self.output_bytes as f64 / self.input_bytes as f64)
    }
}

/// Tunable knobs for the deterministic transforms (see `profiles`).
/// ANSI stripping and CR-collapse always run: they remove formatting noise
/// only and can never change meaning.
#[derive(Debug, Clone)]
pub struct CompressConfig {
    /// Minimum string length (bytes) eligible for compression inside JSON.
    pub min_size: usize,
    /// Consecutive identical lines needed before dedupe collapse.
    pub dedupe_threshold: usize,
    /// Compact lines that are themselves JSON documents.
    pub compact_json_lines: bool,
    /// Collapse runs of blank lines.
    pub collapse_blanks: bool,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            min_size: 2048,
            dedupe_threshold: 3,
            compact_json_lines: true,
            collapse_blanks: true,
        }
    }
}

/// Compress free text with all deterministic transforms (default config).
pub fn compress_text(input: &str) -> (String, CompressOutcome) {
    compress_text_with(input, &CompressConfig::default())
}

/// Compress free text with an explicit config.
pub fn compress_text_with(input: &str, cfg: &CompressConfig) -> (String, CompressOutcome) {
    let mut out = input.to_string();
    let mut applied: Vec<&'static str> = Vec::new();

    // 0. Whole-document JSON compaction (pretty-printed blobs).
    let t = out.trim();
    if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            if let Ok(compact) = serde_json::to_string(&v) {
                if compact.len() < out.len() {
                    out = compact;
                    applied.push("json_document_compact");
                }
            }
        }
    }

    // 1. Strip ANSI escape sequences (colors, cursor movement).
    let stripped = strip_ansi(&out);
    if stripped.len() < out.len() {
        out = stripped;
        applied.push("strip_ansi");
    }

    // 2. Normalize carriage-return overwrites (progress bars): keep last frame.
    let cr_fixed = collapse_cr_overwrites(&out);
    if cr_fixed.len() < out.len() {
        out = cr_fixed;
        applied.push("cr_overwrite");
    }

    // 3. Collapse runs of >=threshold consecutive identical lines to one + marker.
    let (deduped, n_dedup) = collapse_repeated_lines(&out, cfg.dedupe_threshold);
    if n_dedup > 0 {
        out = deduped;
        applied.push("dedupe_lines");
    }

    // 4. Collapse blank-line runs (>=2 blanks -> 1).
    if cfg.collapse_blanks {
        let (blanked, n_blank) = collapse_blank_runs(&out);
        if n_blank > 0 {
            out = blanked;
            applied.push("blank_runs");
        }
    }

    // 5. Compact lines that are themselves JSON documents.
    if cfg.compact_json_lines {
        let (compacted, n_json) = compact_json_lines(&out);
        if n_json > 0 {
            out = compacted;
            applied.push("json_compact");
        }
    }

    // 6. Trim trailing whitespace per line.
    let trimmed = trim_line_ends(&out);
    if trimmed.len() < out.len() {
        out = trimmed;
        applied.push("trim_line_ends");
    }

    let outcome = CompressOutcome {
        input_bytes: input.len(),
        output_bytes: out.len(),
        transforms_applied: applied,
    };
    (out, outcome)
}

/// Strip ANSI escape sequences (CSI ... final byte, and OSC ... BEL/ST).
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI: skip until final byte 0x40..=0x7e
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                i = j + 1;
            } else if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                // OSC: skip until BEL or ESC \
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j;
            } else {
                // Two-byte escape: ESC + single char
                i += 2;
            }
        } else {
            // Copy one UTF-8 char
            let ch_len = utf8_char_len(bytes[i]);
            if i + ch_len <= bytes.len() {
                out.push_str(&input[i..i + ch_len]);
            }
            i += ch_len;
        }
    }
    out
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Lines separated by `\r` (progress bars): keep only the final segment of each line.
pub fn collapse_cr_overwrites(input: &str) -> String {
    input
        .lines()
        .map(|line| match line.rfind('\r') {
            Some(pos) => &line[pos + 1..],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse runs of `threshold` or more consecutive identical non-empty lines.
/// Returns (new_text, number_of_runs_collapsed).
pub fn collapse_repeated_lines(input: &str, threshold: usize) -> (String, usize) {
    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut runs = 0;
    let mut i = 0;
    while i < lines.len() {
        let current = lines[i];
        let mut count = 1;
        while i + count < lines.len() && lines[i + count] == current {
            count += 1;
        }
        if current.trim().is_empty() || count < threshold {
            for _ in 0..count {
                out.push(current.to_string());
            }
        } else {
            runs += 1;
            out.push(current.to_string());
            out.push(format!("… (previous line repeated x{})", count - 1));
        }
        i += count;
    }
    (out.join("\n"), runs)
}

/// Collapse runs of >=2 blank lines into exactly 1. Returns (text, runs_collapsed).
pub fn collapse_blank_runs(input: &str) -> (String, usize) {
    let mut out: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    let mut collapsed = 0usize;
    for line in input.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push(line);
            } else {
                collapsed += 1;
            }
        } else {
            blank_run = 0;
            out.push(line);
        }
    }
    if collapsed > 0 {
        collapsed = 1; // count as one transform application
    }
    (out.join("\n"), collapsed)
}

/// Any line that parses as a JSON object/array gets re-serialized compactly.
pub fn compact_json_lines(input: &str) -> (String, usize) {
    let mut n = 0;
    let out = input
        .lines()
        .map(|line| {
            let t = line.trim();
            if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'))
            {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
                    if let Ok(compact) = serde_json::to_string(&v) {
                        if compact.len() < line.len() {
                            n += 1;
                            return compact;
                        }
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (out, n)
}

fn trim_line_ends(input: &str) -> String {
    input
        .lines()
        .map(|l| l.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Walk a JSON value; compress any string field >= `min_bytes` with compress_text.
/// Returns the transformed value and total bytes saved. Never errors — on any
/// surprise the original value is returned untouched.
pub fn compress_json_value(value: serde_json::Value, min_bytes: usize) -> (serde_json::Value, usize) {
    let cfg = CompressConfig {
        min_size: min_bytes,
        ..CompressConfig::default()
    };
    compress_json_value_with(value, &cfg)
}

/// Config-driven variant of `compress_json_value`.
pub fn compress_json_value_with(
    value: serde_json::Value,
    cfg: &CompressConfig,
) -> (serde_json::Value, usize) {
    let mut saved = 0usize;
    let out = walk(value, cfg, &mut saved);
    (out, saved)
}

fn walk(value: serde_json::Value, cfg: &CompressConfig, saved: &mut usize) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) if s.len() >= cfg.min_size => {
            let (compressed, _outcome) = compress_text_with(&s, cfg);
            if compressed.len() < s.len() {
                *saved += s.len() - compressed.len();
                Value::String(compressed)
            } else {
                Value::String(s)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| walk(v, cfg, saved))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, walk(v, cfg, saved)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_escapes_are_stripped() {
        let input = "\x1b[31merror\x1b[0m: something failed\x1b[K";
        let out = strip_ansi(input);
        assert_eq!(out, "error: something failed");
    }

    #[test]
    fn repeated_lines_collapse_with_count() {
        let mut input = String::new();
        for _ in 0..50 {
            input.push_str("waiting for lock...\n");
        }
        let (out, runs) = collapse_repeated_lines(&input, 3);
        assert_eq!(runs, 1);
        assert!(out.contains("repeated x49"));
        assert!(out.contains("waiting for lock..."));
    }

    #[test]
    fn cr_progress_bars_keep_last_frame() {
        let input = "downloading 10%\rdownloading 55%\rdownloading 100%\n";
        let out = collapse_cr_overwrites(input);
        assert_eq!(out, "downloading 100%");
    }

    #[test]
    fn blank_runs_collapse() {
        let input = "a\n\n\n\n\nb";
        let (out, collapsed) = collapse_blank_runs(input);
        assert_eq!(out, "a\n\nb");
        assert!(collapsed > 0);
    }

    #[test]
    fn json_lines_compact() {
        // Single-line JSON with wasted whitespace (JSONL style).
        let input = "{  \"a\":  1,   \"b\":  [1, 2,   3]  }";
        let (out, n) = compact_json_lines(input);
        assert_eq!(n, 1);
        assert_eq!(out, "{\"a\":1,\"b\":[1,2,3]}");
    }

    #[test]
    fn pretty_json_document_compacts_whole() {
        let input = "{\n  \"a\":  1,\n  \"b\":   [1, 2,  3]\n}";
        let (out, outcome) = compress_text(input);
        assert_eq!(out, "{\"a\":1,\"b\":[1,2,3]}");
        assert!(outcome.transforms_applied.contains(&"json_document_compact"));
    }

    #[test]
    fn ls_style_output_preserves_every_filename() {
        // Realistic `ls -R`-style payload with noise.
        let mut input = String::new();
        input.push_str("\x1b[1m./src\x1b[0m:\n");
        let mut expected_files = Vec::new();
        for i in 0..40 {
            let name = format!("module_{i:03}.rs");
            expected_files.push(name.clone());
            input.push_str(&format!("  {name}\n"));
        }
        for _ in 0..20 {
            input.push_str("\n\n\n");
        }
        input.push_str("build log:\n");
        for _ in 0..30 {
            input.push_str("compiling... 42%\rcompiling... 88%\n");
        }

        let (out, outcome) = compress_text(&input);
        // Every filename survives.
        for f in &expected_files {
            assert!(out.contains(f), "lost filename: {f}");
        }
        // And we actually saved something meaningful.
        assert!(
            outcome.ratio() > 0.15,
            "expected >15% savings, got {:.2}",
            outcome.ratio()
        );
    }

    #[test]
    fn big_repeated_log_hits_40_percent() {
        // The SPEC acceptance case: repetitive structured output >= 40% savings.
        // Real log spam arrives in consecutive identical runs.
        let mut input = String::new();
        for _ in 0..250 {
            input.push_str(
                "[2026-08-27 10:00:12] INFO worker pool heartbeat status=ok latency=3ms\n",
            );
        }
        for _ in 0..250 {
            input.push_str("[2026-08-27 10:04:05] DEBUG cache miss key=session-state\n");
        }
        let (out, outcome) = compress_text(&input);
        assert!(
            outcome.ratio() >= 0.40,
            "expected >=40% savings, got {:.2}",
            outcome.ratio()
        );
        assert!(out.contains("heartbeat status=ok"));
        assert!(out.contains("cache miss key=session-state"));
    }

    #[test]
    fn compress_json_value_saves_bytes_on_tool_output() {
        let payload = serde_json::json!({
            "role": "tool",
            "content": "x".repeat(100) + &"line one\n".repeat(200)
        });
        let (out, saved) = compress_json_value(payload.clone(), 2048);
        // content is 100 + 200*9 = 1900 bytes < 2048 threshold -> untouched
        assert_eq!(out, payload);
        assert_eq!(saved, 0);

        let big = serde_json::json!({
            "content": "x".repeat(100) + &"line one\n".repeat(400)
        });
        let (out2, saved2) = compress_json_value(big, 2048);
        assert!(saved2 > 0, "expected savings on large repeated content");
        let s = out2["content"].as_str().unwrap();
        assert!(s.contains("line one"));
    }

    #[test]
    fn compression_never_grows_input() {
        let samples = vec![
            "",
            "short",
            "{}",
            "no trailing spaces\nhere",
            "\x1b[1mbold\x1b[0m",
        ];
        for s in samples {
            let (out, _) = compress_text(s);
            assert!(out.len() <= s.len(), "grew input: {s:?}");
        }
    }

    #[test]
    fn short_strings_below_threshold_untouched() {
        let payload = serde_json::json!({ "content": "tiny" });
        let (out, saved) = compress_json_value(payload.clone(), 2048);
        assert_eq!(out, payload);
        assert_eq!(saved, 0);
    }

    #[test]
    fn repeated_tool_output_lines_compress_hard() {
        let content = "tool output line\n".repeat(400) + "RESULT: 42\n";
        let (out, outcome) = compress_text(&content);
        assert!(out.len() < content.len() / 10, "expected >90% savings on pure repetition");
        assert!(outcome.transforms_applied.contains(&"dedupe_lines"));
        assert!(out.contains("RESULT: 42"));
    }

    #[test]
    fn nested_chat_response_walk_saves_bytes() {
        let content = "tool output line\n".repeat(400) + "RESULT: 42\n";
        let payload = serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        let (out, saved) = compress_json_value(payload, 2048);
        let got = out["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(saved > 0, "expected savings walking nested payload");
        assert!(got.contains("RESULT: 42"));
    }
}
