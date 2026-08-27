mod compress;
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
        /// Minimum string size (bytes) eligible for compression
        #[arg(long, default_value = "2048")]
        min_size: usize,
        /// SQLite database path for stats
        #[arg(long, default_value = "context-prune.db")]
        db: String,
        /// Disable compression entirely (pure passthrough)
        #[arg(long)]
        passthrough: bool,
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
        } => proxy::serve(port, &upstream, min_size, &db, passthrough).await,
        Command::Stats { db } => stats_cmd(&db),
        Command::Compress {
            path,
            min_size,
            show,
        } => compress_cmd(&path, min_size, show),
        Command::Bench {
            path,
            iterations,
            min_size,
        } => bench_cmd(&path, iterations, min_size),
    }
}

mod proxy;

fn compress_cmd(path: &str, min_size: usize, show: bool) -> Result<()> {
    use std::io::Read;
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
                let (out_v, saved) = compress::compress_json_value(v, min_size);
                let out_str = serde_json::to_string_pretty(&out_v)?;
                let r = if before.is_empty() {
                    0.0
                } else {
                    1.0 - out_str.len() as f64 / input.len() as f64
                };
                (out_str, saved, "json", Vec::new(), r.max(0.0))
            }
            Err(_) => {
                let (out, outcome) = compress::compress_text(&input);
                let saved = outcome.input_bytes.saturating_sub(outcome.output_bytes);
                let ratio = outcome.ratio();
                (out, saved, "text", outcome.transforms_applied, ratio)
            }
        };

    let in_bytes = input.len();
    let out_bytes = output.len();
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
