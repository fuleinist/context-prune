//! Per-model compression profiles (SPEC v2 stretch goal).
//!
//! Different models justify different compression aggressiveness: small
//! context-window models benefit from squeezing harder, while long-context
//! frontier models can afford to keep more verbatim detail. Profiles are
//! deterministic — no heuristics that vary between runs.

use crate::compress::CompressConfig;

/// A named compression profile.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: &'static str,
    pub config: CompressConfig,
}

/// The standard profile: min_size 2048, dedupe threshold 3.
pub fn default_profile() -> Profile {
    Profile {
        name: "default",
        config: CompressConfig::default(),
    }
}

/// Conservative: only compress large blobs (min_size 4096, threshold 5).
pub fn conservative() -> Profile {
    Profile {
        name: "conservative",
        config: CompressConfig {
            min_size: 4096,
            dedupe_threshold: 5,
            ..CompressConfig::default()
        },
    }
}

/// Aggressive: compress from 512 bytes, collapse repeats of 2+.
pub fn aggressive() -> Profile {
    Profile {
        name: "aggressive",
        config: CompressConfig {
            min_size: 512,
            dedupe_threshold: 2,
            ..CompressConfig::default()
        },
    }
}

/// Look up a built-in profile by name.
pub fn resolve(name: &str) -> Option<Profile> {
    match name {
        "default" => Some(default_profile()),
        "conservative" => Some(conservative()),
        "aggressive" => Some(aggressive()),
        _ => None,
    }
}

/// Pick a profile for the model named in a request body.
///
/// Rule: models with small context windows (haiku/flash/mini/nano families)
/// get the aggressive profile — tokens are scarcer there. Everything else
/// falls back to the caller-chosen profile. Unknown/missing model = fallback.
pub fn for_model(model: Option<&str>, fallback: &Profile) -> Profile {
    let Some(m) = model else {
        return fallback.clone();
    };
    let lower = m.to_ascii_lowercase();
    let small_context = ["haiku", "flash", "mini", "nano"]
        .iter()
        .any(|tag| lower.contains(tag));
    if small_context {
        aggressive()
    } else {
        fallback.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::compress_text_with;

    #[test]
    fn resolve_known_profiles() {
        assert_eq!(resolve("default").unwrap().config.min_size, 2048);
        assert_eq!(resolve("conservative").unwrap().config.min_size, 4096);
        assert_eq!(resolve("aggressive").unwrap().config.min_size, 512);
        assert!(resolve("nonexistent").is_none());
    }

    #[test]
    fn small_models_route_to_aggressive() {
        let base = default_profile();
        assert_eq!(
            for_model(Some("claude-haiku-4-20250514"), &base).name,
            "aggressive"
        );
        assert_eq!(
            for_model(Some("gemini-2.5-flash"), &base).name,
            "aggressive"
        );
        assert_eq!(for_model(Some("gpt-5-mini"), &base).name, "aggressive");
    }

    #[test]
    fn large_models_keep_fallback() {
        let base = default_profile();
        assert_eq!(for_model(Some("gpt-5"), &base).name, "default");
        assert_eq!(for_model(Some("claude-opus-4"), &base).name, "default");
        assert_eq!(for_model(None, &base).name, "default");
        let cons = conservative();
        assert_eq!(for_model(Some("gpt-5"), &cons).name, "conservative");
    }

    #[test]
    fn aggressive_compresses_what_default_skips() {
        // ~700 bytes of repeated lines: above aggressive's 512 floor,
        // below conservative's 4096.
        let input = "heartbeat ok latency=3ms\n".repeat(30);
        let (out_agg, _) = compress_text_with(&input, &aggressive().config);
        assert!(
            out_agg.len() < input.len() / 5,
            "aggressive should crush repeated lines"
        );
        assert!(out_agg.contains("heartbeat ok"));
    }

    #[test]
    fn dedupe_threshold_respected() {
        // Exactly 3 repeats: collapsed under default (threshold 3),
        // kept verbatim under conservative (threshold 5).
        let input = "same line\nsame line\nsame line\nother\n";
        let def = default_profile();
        let cons = conservative();
        let (out_def, _) = compress_text_with(input, &def.config);
        assert!(out_def.contains("repeated x2"));
        let (out_con, _) = compress_text_with(input, &cons.config);
        assert!(!out_con.contains("repeated x2"));
    }
}
