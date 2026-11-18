//! Command-line access to the index daemon, without GTK (PLAN §5.8):
//!
//! ```text
//! tudi q [--json] [-n N] [--dir] [--hidden] TERMS...
//! tudi index status|rescan|stop
//! ```
//!
//! The daemon is started (next to this binary) if it is not running.

// This module's output is the CLI's output.
#![allow(clippy::print_stdout)]

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

use tudi_proto::{Client, Request, Response};

/// Handle `q` / `index`; `None` if `args` is not a CLI command.
pub fn run(args: &[String]) -> Option<u8> {
    match args.first().map(String::as_str) {
        Some("q") => Some(query(&args[1..])),
        Some("index") => Some(index(&args[1..])),
        _ => None,
    }
}

fn start_daemon() {
    let exe = std::env::current_exe()
        .map(|p| p.with_file_name("tudi-indexd"))
        .unwrap_or_else(|_| "tudi-indexd".into());
    let _ = std::process::Command::new(exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

/// Connect, starting the daemon if needed (waits up to 5 s).
fn connect(start: bool) -> Option<Client> {
    if let Ok(c) = Client::connect() {
        return Some(c);
    }
    if !start {
        return None;
    }
    start_daemon();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(c) = Client::connect() {
            return Some(c);
        }
    }
    None
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn usage() -> u8 {
    eprintln!("usage: tudi q [--json] [-n N] [--dir] [--hidden] TERMS...");
    2
}

fn query(args: &[String]) -> u8 {
    let (mut json, mut dirs, mut hidden, mut limit) = (false, false, false, 20u32);
    let mut terms = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--dir" | "-d" => dirs = true,
            "--hidden" => hidden = true,
            "-n" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => limit = n,
                None => return usage(),
            },
            _ => terms.push(a.clone()),
        }
    }
    if terms.is_empty() {
        return usage();
    }
    let Some(mut c) = connect(true) else {
        eprintln!("tudi: cannot reach the index daemon (tudi-indexd)");
        return 1;
    };
    let req = Request::Query {
        id: 1,
        text: terms.join(" "),
        limit,
        show_hidden: hidden,
        dirs_only: dirs,
    };
    let Ok(Response::Results { items, partial, .. }) = c.call(&req) else {
        eprintln!("tudi: query failed");
        return 1;
    };
    if partial {
        eprintln!("tudi: the index is still being built; results may be incomplete");
    }
    let out = std::io::stdout();
    let mut out = out.lock();
    for it in &items {
        if json {
            let kind = match it.kind {
                1 => "dir",
                2 => "symlink",
                3 => "other",
                _ => "file",
            };
            let _ = writeln!(
                out,
                "{{\"path\":{},\"kind\":\"{kind}\",\"size\":{},\"mtime\":{},\"score\":{:.2}}}",
                json_str(&String::from_utf8_lossy(&it.path)),
                it.size,
                it.mtime,
                it.score
            );
        } else {
            let _ = out.write_all(&it.path);
            let _ = out.write_all(b"\n");
        }
    }
    u8::from(items.is_empty())
}

fn index(args: &[String]) -> u8 {
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    let req = match cmd {
        "status" => Request::Status,
        "rescan" => Request::Rescan,
        "stop" => Request::Stop,
        _ => {
            eprintln!("usage: tudi index status|rescan|stop");
            return 2;
        }
    };
    let Some(mut c) = connect(cmd != "stop") else {
        if cmd == "stop" {
            return 0; // not running
        }
        eprintln!("tudi: cannot reach the index daemon (tudi-indexd)");
        return 1;
    };
    match c.call(&req) {
        Ok(Response::Status(s)) => {
            println!("state:    {}", s.state);
            println!("entries:  {}", s.entries);
            println!("roots:    {}", s.roots.join(", "));
            println!("memory:   {:.0} MB", s.rss_mb);
            0
        }
        Ok(Response::Ok) => 0,
        Ok(r) => {
            eprintln!("tudi: unexpected reply {r:?}");
            1
        }
        Err(e) => {
            eprintln!("tudi: {e}");
            1
        }
    }
}
