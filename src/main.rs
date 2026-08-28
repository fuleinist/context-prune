mod compress;
mod cache;
mod profiles;
#[cfg(feature = "skeleton")]
mod skeleton;
mod stats;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "context-prune",
    version,
    about = "Token compression proxy for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the OpenAI-compatible compression proxy
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "8787")]
        port: u16,
        /// Upstream LLM API base URL
        #[arg(long, default_value = "https://api.openai.com")]
        upstream: String,
        /// Minimum string size (bytes) eligible for compression;
        /// overrides the profile's min_size when given
        #[arg(long)]
        min_size: Option<usize>,
        /// SQLite database path for stats
        #[arg(long, default_value = "context-prune.db")]
        db: String,
        /// Disable compression entirely (pure passthrough)
        #[arg(long)]
        passthrough: bool,
        /// Compression profile: default, conservative, aggressive
        /// (per-model overrides still apply, e.g. small models -> aggressive)
        #[arg(long, default_value = "default")]
        profile: String,
        /// Disable the content-hash compression cache
        #[arg(long, default_value = "false")]
        no_cache: bool,
    },
    /// Show cumulative compression stats from the DB
    Stats {
        /// SQLite database path
        #[arg(long, default_value = "context-prune.db")]
        db: String,
    },
    /// Benchmark compression throughput on a file (debug vs release comparison)
    Bench {
        /// File to compress
        path: String,
        /// Number of iterations to run
        #[arg(long, default_value = "200")]
        iterations: usize,
        /// Minimum string size (bytes) eligible for compression (JSON mode)
        #[arg(long, default_value = "2048")]
        min_size: usize,
    },
    /// Skeletonize a Rust source file: keep signatures, drop bodies (SPEC v2)
    #[cfg(feature = "skeleton")]
    Skeleton {
        /// File to skeletonize
        path: String,
        /// Also print the skeleton output
        #[arg(long)]
        show: bool,
    },
    /// Compress a single file and report the ratio (demo / debugging)
    Compress {
        /// File to compress ('-' for stdin)
        path: String,
        /// Minimum string size eligible for compression (for JSON mode)
        #[arg(long, default_value = "2048")]
        min_size: usize,
        /// Also print the compressed output
        #[arg(long)]
        show: bool,
        /// Compression profile: default, conservative, aggressive
        #[arg(long, default_value = "default")]
        profile: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            port,
            upstream,
            min_size,
            db,
            passthrough,
            profile,
            no_cache,
        } => proxy::serve(port, &upstream, &profile, min_size, &db, passthrough, no_cache).await,
        Command::Stats { db } => stats_cmd(&db),
        Command::Compress {
            path,
            min_size,
            show,
            profile,
        } => compress_cmd(&path, min_size, show, &profile),
        Command::Bench {
            path,
            iterations,
            min_size,
        } => bench_cmd(&path, iterations, min_size),
        #[cfg(feature = "skeleton")]
        Command::Skeleton { path, show } => skeleton_cmd(&path, show),
    }
}

mod proxy;

fn compress_cmd(path: &str, min_size: usize, show: bool, profile_name: &str) -> Result<()> {
    use std::io::Read;
    let profile = profiles::resolve(profile_name)
        .ok_or_else(|| anyhow::anyhow!("unknown profile '{profile_name}' (try: default, conservative, aggressive)"))?;
    let cfg = compress::CompressConfig {
        min_size,
        ..profile.config
    };
    let input = if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(path)?
    };

    // Try JSON-document mode first, fall back to plain text.
    let (output, saved, mode, transforms, ratio) =
        match serde_json::from_str::<serde_json::Value>(&input) {
            Ok(v) => {
                let before = serde_json::to_string(&v)?;
                let (out_v, saved) = compress::compress_json_value_with(v, &cfg);
                let out_str = serde_json::to_string_pretty(&out_v)?;
                let r = if before.is_empty() {
                    0.0
                } else {
                    1.0 - out_str.len() as f64 / input.len() as f64
                };
                (out_str, saved, "json", Vec::new(), r.max(0.0))
            }
            Err(_) => {
                let (out, outcome) = compress::compress_text_with(&input, &cfg);
                let saved = outcome.input_bytes.saturating_sub(outcome.output_bytes);
                let ratio = outcome.ratio();
                (out, saved, "text", outcome.transforms_applied, ratio)
            }
        };

    let in_bytes = input.len();
    let out_bytes = output.len();
    println!("profile:         {profile_name}");
    println!("mode:            {mode}");
    if !transforms.is_empty() {
        println!("transforms:      {}", transforms.join(", "));
    }
    println!("input:           {in_bytes} bytes");
    println!("output:          {out_bytes} bytes");
    println!("saved:           {saved} bytes ({:.1}%)", ratio * 100.0);
    if show {
        println!("---");
        println!("{output}");
    }
    Ok(())
}

fn bench_cmd(path: &str, iterations: usize, min_size: usize) -> Result<()> {
    if iterations == 0 {
        anyhow::bail!("iterations must be > 0");
    }
    let input = std::fs::read_to_string(path)?;
    anyhow::ensure!(!input.is_empty(), "input file is empty");
    let is_json = serde_json::from_str::<serde_json::Value>(&input).is_ok();

    let mut out_bytes = 0usize;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        out_bytes = if is_json {
            let v: serde_json::Value = serde_json::from_str(&input)?;
            let (out_v, _) = compress::compress_json_value(v, min_size);
            serde_json::to_string(&out_v)?.len()
        } else {
            let (out, _) = compress::compress_text(&input);
            out.len()
        };
    }
    let elapsed = start.elapsed();

    let mode = if is_json { "json" } else { "text" };
    let ratio = 1.0 - out_bytes as f64 / input.len() as f64;
    let mbps =
        (input.len() as f64 * iterations as f64) / 1_048_576.0 / elapsed.as_secs_f64();
    let build = if cfg!(debug_assertions) { "debug" } else { "release" };

    println!("build:           {build}");
    println!("mode:            {mode}");
    println!("input:           {} bytes", input.len());
    println!("output:          {out_bytes} bytes");
    println!("savings:         {:.1}%", ratio * 100.0);
    println!("iterations:      {iterations}");
    println!("elapsed:         {:.3}s", elapsed.as_secs_f64());
    println!("per-iter:        {:.3} ms", elapsed.as_secs_f64() / iterations as f64 * 1000.0);
    println!("throughput:      {mbps:.1} MB/s");
    Ok(())
}

#[cfg(feature = "skeleton")]
fn skeleton_cmd(path: &str, show: bool) -> Result<()> {
    let input = std::fs::read_to_string(path)?;
    match skeleton::rust_skeleton(&input) {
        Some(out) => {
            let ratio = if input.is_empty() {
                0.0
            } else {
                1.0 - out.len() as f64 / input.len() as f64
            };
            println!("mode:            code-skeleton (rust, tree-sitter)");
            println!("input:           {} bytes", input.len());
            println!("output:          {} bytes", out.len());
            println!("saved:           {:.1}%", ratio * 100.0);
            if show {
                println!("---");
                println!("{out}");
            }
        }
        // F5 safety: parse failure = passthrough, never break.
        None => {
            println!("mode:            passthrough (input did not parse as Rust)");
            println!("input:           {} bytes", input.len());
        }
    }
    Ok(())
}

fn stats_cmd(db: &str) -> Result<()> {
    let s = stats::StatsStore::open(db)?;
    let summary = s.summary()?;
    println!(
        "requests:        {}",
        summary.requests
    );
    println!("bytes in:        {}", summary.bytes_in);
    println!("bytes out:       {}", summary.bytes_out);
    println!(
        "bytes saved:     {}",
        summary.bytes_in.saturating_sub(summary.bytes_out)
    );
    if summary.bytes_in > 0 {
        let ratio = 1.0 - summary.bytes_out as f64 / summary.bytes_in as f64;
        println!("avg savings:     {:.1}%", ratio * 100.0);
    }
    Ok(())
}
