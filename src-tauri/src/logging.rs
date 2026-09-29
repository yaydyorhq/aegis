//! Append-only file log next to `aegis.db`.
//!
//! Release builds are `panic = "abort"` with stripped symbols and no console,
//! so without this there is no post-mortem trail when a mint or fund job dies.
//! Every line goes through `redact_urls_in` first — RPC URLs carry API keys.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static PATH: OnceLock<PathBuf> = OnceLock::new();

/// Keep the log bounded: rotate the previous file away once it is large.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub fn init(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() >= MAX_BYTES {
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
    }
    let _ = PATH.set(path);
}

pub fn info(ctx: &str, msg: &str) {
    write_line("INFO ", ctx, msg);
}

pub fn warn(ctx: &str, msg: &str) {
    write_line("WARN ", ctx, msg);
}

pub fn error(ctx: &str, msg: &str) {
    write_line("ERROR", ctx, msg);
}

fn write_line(level: &str, ctx: &str, msg: &str) {
    let Some(path) = PATH.get() else {
        return; // init not called (unit tests) — activity table still records it
    };
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let msg = crate::chain::redact_urls_in(msg);
    let line = format!("{ts} {level} {ctx} | {msg}\n");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}
