//! Frame monitoring and GUI benchmarks (PLAN §9.2).
//!
//! Always on: every window's frame clock is watched; a frame whose work
//! (before-paint → after-paint) exceeds 16 ms is logged with `tracing` and
//! counted ([`frame_stats`]), for the M3 "no main-thread stall" check.
//!
//! `TUDI_BENCH=<scenario>` runs one measurement, prints a JSON line to
//! stdout and quits:
//!
//! - `startup`: process start → first painted frame (P1b).
//! - `reopen`: close the first window, keep the process, open a new window;
//!   request → its first painted frame (P1a).
//! - `open:PATH[:N]`: navigate to PATH N times (default 10), alternating
//!   with its parent; navigation → first painted frame with the rows (P3/P4).

// Results go to stdout on purpose: they are read by benchmark scripts.
#![allow(clippy::print_stdout)]

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::browser::Browser;

/// Process start, set first thing in `main`.
pub static START: OnceLock<Instant> = OnceLock::new();

const LONG_FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub frames: u64,
    pub long_frames: u64,
    pub max_ms: f64,
}

/// Per-frame counters of list row work, reset after each frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct RowWork {
    pub setups: u32,
    pub binds: u32,
    pub bind_us: u64,
}

thread_local! {
    pub static ROW_WORK: Cell<RowWork> = const { Cell::new(RowWork { setups: 0, binds: 0, bind_us: 0 }) };
    static STATS: Cell<FrameStats> = const { Cell::new(FrameStats { frames: 0, long_frames: 0, max_ms: 0.0 }) };
    static BENCH: RefCell<Option<Bench>> = const { RefCell::new(None) };
}

pub fn frame_stats() -> FrameStats {
    STATS.with(|s| s.get())
}

enum Bench {
    Startup,
    /// First window shown; waiting to close it.
    ReopenFirst,
    /// First window is being closed; the new one is not requested yet.
    ReopenWaiting,
    /// Reopen requested at this instant.
    ReopenPending(Instant),
    Open {
        path: PathBuf,
        runs: usize,
        samples: Vec<f64>,
        /// Navigation to `path` (true) or to its parent started at.
        pending: Option<(bool, Instant)>,
    },
}

/// Read `TUDI_BENCH` once at startup.
pub fn init() {
    let Ok(spec) = std::env::var("TUDI_BENCH") else {
        return;
    };
    let bench = match spec.split_once(':') {
        None if spec == "startup" => Bench::Startup,
        None if spec == "reopen" => Bench::ReopenFirst,
        Some(("open", rest)) => {
            let (path, runs) = match rest.rsplit_once(':') {
                Some((p, n)) if n.parse::<usize>().is_ok() => (p, n.parse().unwrap()),
                _ => (rest, 10),
            };
            Bench::Open {
                path: PathBuf::from(path),
                runs,
                samples: Vec::new(),
                pending: None,
            }
        }
        _ => {
            tracing::error!("unknown TUDI_BENCH scenario {spec}");
            return;
        }
    };
    BENCH.with(|b| *b.borrow_mut() = Some(bench));
}

/// Watch a window's frames. `reopen` opens a new window (for `reopen`).
pub fn attach(b: &Rc<Browser>, reopen: Rc<dyn Fn()>) {
    let bw = Rc::downgrade(b);
    let hook = move |w: &adw::ApplicationWindow| {
        let clock = w.frame_clock().expect("realized window has a frame clock");
        let before = Rc::new(Cell::new(None::<Instant>));
        let bf = before.clone();
        clock.connect_before_paint(move |_| bf.set(Some(Instant::now())));
        let (bw, reopen) = (bw.clone(), reopen.clone());
        clock.connect_after_paint(move |_| {
            let now = Instant::now();
            if let Some(t) = before.take() {
                record_frame(now - t);
            }
            if let Some(b) = bw.upgrade() {
                on_paint(&b, now, &reopen);
            }
        });
    };
    // `present()` may already have realized the window.
    if b.window.is_realized() {
        hook(&b.window);
    } else {
        b.window.connect_realize(move |w| hook(w));
    }
}

fn record_frame(work: Duration) {
    let ms = work.as_secs_f64() * 1000.0;
    let rw = ROW_WORK.with(|r| r.replace(RowWork::default()));
    STATS.with(|s| {
        let mut st = s.get();
        st.frames += 1;
        st.max_ms = st.max_ms.max(ms);
        if work > LONG_FRAME {
            st.long_frames += 1;
            let at = START
                .get()
                .map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
            tracing::warn!(
                ms,
                at,
                setups = rw.setups,
                binds = rw.binds,
                bind_ms = rw.bind_us as f64 / 1000.0,
                "long frame on the main thread"
            );
        }
        s.set(st);
    });
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn quit(b: &Browser) {
    if let Some(app) = b.window.application() {
        app.quit();
    }
}

fn on_paint(b: &Rc<Browser>, now: Instant, reopen: &Rc<dyn Fn()>) {
    BENCH.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(bench) = slot.as_mut() else { return };
        match bench {
            Bench::Startup => {
                let t = ms(now - *START.get().unwrap());
                println!("{{\"bench\":\"startup\",\"ms\":{t:.2}}}");
                *slot = None;
                quit(b);
            }
            Bench::ReopenFirst => {
                *bench = Bench::ReopenWaiting;
                let (w, reopen) = (b.window.clone(), reopen.clone());
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    w.close();
                    glib::timeout_add_local_once(Duration::from_millis(500), move || {
                        BENCH.with(|c| {
                            *c.borrow_mut() = Some(Bench::ReopenPending(Instant::now()))
                        });
                        reopen();
                    });
                });
            }
            Bench::ReopenWaiting => {}
            Bench::ReopenPending(t0) => {
                // The first window is gone: this is the new window's first
                // frame (P1a).
                let t = ms(now - *t0);
                println!("{{\"bench\":\"reopen\",\"ms\":{t:.2}}}");
                *slot = None;
                quit(b);
            }
            Bench::Open {
                path,
                runs,
                samples,
                pending,
            } => {
                let shown = b.tab().pane.n_items() > 0 && *b.tab().current.borrow() == *path;
                match *pending {
                    Some((true, t0)) if shown && b.pane_settled() => {
                        samples.push(ms(now - t0));
                        *pending = None;
                    }
                    Some((false, _)) if b.pane_settled() => *pending = None,
                    Some(_) => return,
                    None => {}
                }
                if samples.len() >= *runs {
                    let mut s = samples.clone();
                    s.sort_by(f64::total_cmp);
                    println!(
                        "{{\"bench\":\"open\",\"path\":{:?},\"rows\":{},\"runs\":{},\"median_ms\":{:.2},\"max_ms\":{:.2}}}",
                        path.display().to_string(),
                        b.tab().pane.n_items(),
                        s.len(),
                        s[s.len() / 2],
                        s[s.len() - 1]
                    );
                    *slot = None;
                    quit(b);
                    return;
                }
                // Alternate: parent, then the target.
                let to_target = b.tab().current.borrow().as_path() != path.as_path();
                let next = if to_target {
                    path.clone()
                } else {
                    path.parent().unwrap_or(path).to_path_buf()
                };
                *pending = Some((to_target, Instant::now()));
                let b = b.clone();
                glib::idle_add_local_once(move || b.navigate(&next, false));
            }
        }
    });
}
