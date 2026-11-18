//! Command-line access to the index daemon, without GTK (PLAN §5.8):
//!
//! ```text
//! tudi q [--json] [-n N] [--dir] [--hidden] [--in DIR] [--] QUERY...
//! tudi index status|rescan|stop
//! tudi visit DIR                # record a visit in the folder history
//! tudi init bash|zsh|fish [--cmd NAME] [--hook]
//! ```
//!
//! QUERY uses the search syntax (`docs/query-syntax.md`): `合同 .pdf 上月`.
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
        Some("visit") => Some(visit(&args[1..])),
        Some("init") => Some(init(&args[1..])),
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
    eprintln!("usage: tudi q [--json] [-n N] [--dir] [--hidden] [--in DIR] [--] QUERY...");
    2
}

fn query(args: &[String]) -> u8 {
    let (mut json, mut dirs, mut hidden, mut limit) = (false, false, false, 20u32);
    let mut scope = None;
    let mut terms = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                terms.extend(it.by_ref().cloned());
                break;
            }
            "--in" => match it.next() {
                Some(d) => scope = Some(absolute(d)),
                None => return usage(),
            },
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
        scope: scope.map(|p| p.into_os_string().into_encoded_bytes()),
    };
    let (items, partial, hints) = match c.call(&req) {
        Ok(Response::Results {
            items,
            partial,
            hints,
            ..
        }) => (items, partial, hints),
        Ok(Response::Error(e)) => {
            eprintln!("tudi: {e}");
            return 1;
        }
        _ => {
            eprintln!("tudi: query failed");
            return 1;
        }
    };
    for h in hints {
        eprintln!("tudi: {h} (ignored)");
    }
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

/// `p` made absolute against the current folder (not resolving links).
fn absolute(p: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(p);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    std::env::current_dir().map_or_else(|_| p.to_path_buf(), |c| c.join(p))
}

/// `tudi visit DIR`: count a visit, like opening the folder in tudi
/// (history exclusions apply). Used by the shell hook.
fn visit(args: &[String]) -> u8 {
    let dir = match args {
        [d] => d,
        [dd, d] if dd == "--" => d,
        _ => {
            eprintln!("usage: tudi visit DIR");
            return 2;
        }
    };
    let dir = absolute(dir);
    if !dir.is_dir() {
        return 1;
    }
    let (config, _) = tudi_core::config::Config::load_user();
    let rules = tudi_core::frecency::ExcludeRules::new(&config.history.exclude).unwrap_or_default();
    let Some(db) = tudi_core::frecency::default_path() else {
        return 1;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let recorded = tudi_core::frecency::Frecency::open(&db, rules)
        .and_then(|mut f| f.visit(&dir, now))
        .unwrap_or_else(|e| {
            eprintln!("tudi: {e}");
            false
        });
    if recorded && let Some(mut c) = connect(false) {
        let _ = c.call(&Request::FrecencyChanged);
    }
    0
}

const INIT_POSIX: &str = r#"# tudi shell integration: eval "$(tudi init SHELL)"
__CMD__() {
    if [ $# -eq 0 ]; then cd ~ || return; return; fi
    __tudi_d="$(command tudi q --dir -n 1 -- "$@")" && [ -n "$__tudi_d" ] && cd -- "$__tudi_d"
}
"#;

const HOOK_BASH: &str = r#"__tudi_pwd="$PWD"
__tudi_hook() {
    if [ "$__tudi_pwd" != "$PWD" ]; then
        __tudi_pwd="$PWD"
        (command tudi visit -- "$PWD" >/dev/null 2>&1 &)
    fi
}
case ";${PROMPT_COMMAND:-};" in
    *";__tudi_hook;"*) ;;
    *) PROMPT_COMMAND="__tudi_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
"#;

const HOOK_ZSH: &str = r#"__tudi_hook() { command tudi visit -- "$PWD" >/dev/null 2>&1 &! }
autoload -Uz add-zsh-hook
add-zsh-hook chpwd __tudi_hook
"#;

const INIT_FISH: &str = r#"# tudi shell integration: tudi init fish | source
function __CMD__
    if test (count $argv) -eq 0
        cd ~
        return
    end
    set -l d (command tudi q --dir -n 1 -- $argv)
    and test -n "$d"
    and cd -- $d
end
"#;

const HOOK_FISH: &str = r#"function __tudi_hook --on-variable PWD
    status --is-command-substitution; and return
    command tudi visit -- $PWD >/dev/null 2>&1 &
    disown 2>/dev/null
end
"#;

/// The shell code for `tudi init`.
fn init_script(shell: &str, cmd: &str, hook: bool) -> Option<String> {
    let (base, hook_code) = match shell {
        "bash" => (INIT_POSIX, HOOK_BASH),
        "zsh" => (INIT_POSIX, HOOK_ZSH),
        "fish" => (INIT_FISH, HOOK_FISH),
        _ => return None,
    };
    let mut out = base.replace("__CMD__", cmd);
    if hook {
        out.push_str(hook_code);
    }
    Some(out)
}

/// `tudi init bash|zsh|fish [--cmd NAME] [--hook]`: print shell code
/// defining a jump function (`tj acme` → cd to the best folder match) and,
/// with `--hook`, recording each `cd` in the folder history.
fn init(args: &[String]) -> u8 {
    let mut shell = None;
    let (mut cmd, mut hook) = ("tj".to_string(), false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hook" => hook = true,
            "--cmd" => match it.next() {
                Some(c)
                    if !c.is_empty()
                        && c.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
                {
                    cmd = c.clone();
                }
                _ => shell = Some(String::new()),
            },
            s => shell = Some(s.to_string()),
        }
    }
    match shell.as_deref().and_then(|s| init_script(s, &cmd, hook)) {
        Some(code) => {
            print!("{code}");
            0
        }
        None => {
            eprintln!("usage: tudi init bash|zsh|fish [--cmd NAME] [--hook]");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_scripts() {
        let z = init_script("zsh", "j", true).unwrap();
        assert!(z.contains("j() {") && z.contains("add-zsh-hook chpwd"));
        let b = init_script("bash", "tj", false).unwrap();
        assert!(b.contains("tj() {") && !b.contains("PROMPT_COMMAND"));
        assert!(
            init_script("fish", "tj", true)
                .unwrap()
                .contains("--on-variable PWD")
        );
        assert!(init_script("tcsh", "tj", false).is_none());
    }
}
