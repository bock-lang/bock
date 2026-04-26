//! `bock-dump-vocab` CLI — emits the full vocabulary JSON document.
//!
//! Usage:
//!   bock-dump-vocab              # writes compact JSON to stdout
//!   bock-dump-vocab --pretty     # writes formatted JSON to stdout
//!   bock-dump-vocab -o vocab.json
//!   bock-dump-vocab --pretty -o vocab.json

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Emit the Bock compiler's vocabulary as JSON.
#[derive(Parser)]
#[command(name = "bock-dump-vocab", version, about)]
struct Args {
    /// Write pretty (indented) JSON instead of a single-line document.
    #[arg(long)]
    pretty: bool,

    /// Output file. When omitted, writes to stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let vocab = bock_vocab::build_vocab();

    let json = if args.pretty {
        match serde_json::to_string_pretty(&vocab) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bock-dump-vocab: serialize failed: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        match serde_json::to_string(&vocab) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("bock-dump-vocab: serialize failed: {e}");
                return ExitCode::from(2);
            }
        }
    };

    match args.output {
        Some(path) => {
            if let Err(e) = fs::write(&path, json.as_bytes()) {
                eprintln!("bock-dump-vocab: writing {}: {e}", path.display());
                return ExitCode::from(1);
            }
            // One trailing newline for POSIX-friendly output files.
            if let Err(e) = fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(b"\n"))
            {
                eprintln!("bock-dump-vocab: trailing newline: {e}");
                return ExitCode::from(1);
            }
        }
        None => {
            let mut stdout = io::stdout().lock();
            if let Err(e) = stdout.write_all(json.as_bytes()).and_then(|()| stdout.write_all(b"\n"))
            {
                eprintln!("bock-dump-vocab: stdout: {e}");
                return ExitCode::from(1);
            }
        }
    }

    ExitCode::SUCCESS
}
