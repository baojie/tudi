//! Automated measurement mode, driven by the window's `GdkFrameClock`.
//!
//! - startup: `main()` → first `after-paint`.
//! - reopen (P1a): after the first window has painted, destroy it while the
//!   app stays alive (like a resident instance), wait, then time building a
//!   new window over the same model → its first `after-paint`.
//! - scroll: move the vertical adjustment at a constant speed through the
//!   whole list; record frame intervals (before-paint → before-paint) and
//!   frame work time (before-paint → after-paint).
//! - insert: from a main-loop callback between frames (like an input event),
//!   splice N rows into the visible area and measure the time to the next
//!   `after-paint`; then remove them (collapse) and measure the same.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;

use crate::Options;
use crate::model::RowModel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Startup,
    Reopen,
    /// The second window of `Reopen`; its first paint ends the run.
    Reopened,
    All,
}

thread_local! {
    /// When the reopen request was issued (P1a start).
    static REOPEN_T0: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    static REOPEN_MS: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug)]
enum Phase {
    WaitFirstPaint,
    Settle { frames: u32, then: Box<Phase> },
    Scroll,
    InsertArm { rep: usize },
    InsertPending { rep: usize, t0: Instant },
    RemoveArm { rep: usize },
    RemovePending { rep: usize, t0: Instant },
    Done,
}

#[derive(Default)]
struct Results {
    startup_ms: f64,
    first_paint_epoch_ns: u128,
    intervals_ms: Vec<f64>,
    work_ms: Vec<f64>,
    insert_splice_ms: Vec<f64>,
    insert_to_paint_ms: Vec<f64>,
    remove_splice_ms: Vec<f64>,
    remove_to_paint_ms: Vec<f64>,
}

struct State {
    phase: Phase,
    last_before: Option<Instant>,
    before: Option<Instant>,
    insert_at: usize,
    res: Results,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[i]
}

fn stats(name: &str, v: &[f64], out: &mut String) {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let mean = s.iter().sum::<f64>() / s.len().max(1) as f64;
    let _ = write!(
        out,
        "\"{name}\":{{\"n\":{},\"mean\":{mean:.3},\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"max\":{:.3}}}",
        s.len(),
        pct(&s, 50.0),
        pct(&s, 95.0),
        pct(&s, 99.0),
        s.last().copied().unwrap_or(f64::NAN),
    );
}

pub fn install(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    scrolled: &gtk::ScrolledWindow,
    model: &RowModel,
    opts: &Options,
    mode: Mode,
) {
    let state = Rc::new(RefCell::new(State {
        phase: Phase::WaitFirstPaint,
        last_before: None,
        before: None,
        insert_at: 0,
        res: Results::default(),
    }));
    let opts = opts.clone();
    let (app, scrolled, model) = (app.clone(), scrolled.clone(), model.clone());

    // The frame clock only ticks when something changes; keep it running
    // until the benchmark is done so "wait N frames" always progresses.
    let st = state.clone();
    window.add_tick_callback(move |_, _| {
        if matches!(st.borrow().phase, Phase::Done) {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    window.connect_realize(move |window| {
        let clock = window
            .frame_clock()
            .expect("realized window has a frame clock");

        let st = state.clone();
        clock.connect_before_paint(move |_| {
            let now = Instant::now();
            let mut s = st.borrow_mut();
            if matches!(s.phase, Phase::Scroll)
                && let Some(prev) = s.last_before
            {
                let d = ms(now - prev);
                s.res.intervals_ms.push(d);
            }
            s.last_before = Some(now);
            s.before = Some(now);
        });

        let st = state.clone();
        let (app, scrolled, model, opts) =
            (app.clone(), scrolled.clone(), model.clone(), opts.clone());
        let window = window.clone();
        clock.connect_after_paint(move |_| {
            let now = Instant::now();
            on_after_paint(&st, now, &app, &window, &scrolled, &model, &opts, mode);
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn on_after_paint(
    st: &Rc<RefCell<State>>,
    now: Instant,
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    scrolled: &gtk::ScrolledWindow,
    model: &RowModel,
    opts: &Options,
    mode: Mode,
) {
    let mut s = st.borrow_mut();
    let phase = std::mem::replace(&mut s.phase, Phase::Done);
    if std::env::var_os("SPIKE_TRACE").is_some()
        && !matches!(phase, Phase::Settle { .. } | Phase::Scroll)
    {
        eprintln!("after-paint in {phase:?}");
    }
    s.phase = match phase {
        Phase::WaitFirstPaint => {
            crate::mark("first_after_paint");
            s.res.startup_ms = ms(now - *crate::START.get().unwrap());
            s.res.first_paint_epoch_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            if mode == Mode::Startup {
                finish(&s.res, window, opts, mode);
                app.quit();
                Phase::Done
            } else if mode == Mode::Reopen {
                reopen(app, window, model, opts);
                Phase::Done
            } else if mode == Mode::Reopened {
                let t0 = REOPEN_T0.with(|c| c.get()).expect("reopen start recorded");
                REOPEN_MS.with(|c| c.set(Some(ms(now - t0))));
                finish(&s.res, window, opts, mode);
                app.quit();
                Phase::Done
            } else {
                Phase::Settle {
                    frames: 30,
                    then: Box::new(Phase::Scroll),
                }
            }
        }
        Phase::Settle { frames, then } => {
            if std::env::var_os("SPIKE_TRACE").is_some() && frames.is_multiple_of(10) {
                eprintln!("settle {frames}");
            }
            if frames > 0 {
                Phase::Settle {
                    frames: frames - 1,
                    then,
                }
            } else {
                match *then {
                    Phase::Scroll => start_scroll(st, scrolled, opts.speed),
                    Phase::InsertArm { rep } => arm(st, model, opts, rep, true),
                    Phase::RemoveArm { rep } => arm(st, model, opts, rep, false),
                    _ => {}
                }
                *then
            }
        }
        Phase::Scroll => {
            if let Some(b) = s.before {
                let d = ms(now - b);
                s.res.work_ms.push(d);
            }
            let adj = scrolled.vadjustment();
            if std::env::var_os("SPIKE_TRACE").is_some() && s.res.work_ms.len().is_multiple_of(60) {
                eprintln!(
                    "scroll value={} upper={} page={}",
                    adj.value(),
                    adj.upper(),
                    adj.page_size()
                );
            }
            if adj.value() >= adj.upper() - adj.page_size() - 1.0 {
                // Jump to a position in the middle of the list for inserts.
                adj.set_value(adj.upper() / 2.0);
                Phase::Settle {
                    frames: 20,
                    then: Box::new(Phase::InsertArm { rep: 0 }),
                }
            } else {
                Phase::Scroll
            }
        }
        Phase::InsertPending { rep, t0 } => {
            s.res.insert_to_paint_ms.push(ms(now - t0));
            Phase::Settle {
                frames: 20,
                then: Box::new(Phase::RemoveArm { rep }),
            }
        }
        Phase::RemovePending { rep, t0 } => {
            s.res.remove_to_paint_ms.push(ms(now - t0));
            if rep + 1 < opts.inserts {
                Phase::Settle {
                    frames: 20,
                    then: Box::new(Phase::InsertArm { rep: rep + 1 }),
                }
            } else {
                finish(&s.res, window, opts, mode);
                app.quit();
                Phase::Done
            }
        }
        p => p,
    };
}

fn start_scroll(st: &Rc<RefCell<State>>, scrolled: &gtk::ScrolledWindow, speed: f64) {
    let start: RefCell<Option<i64>> = RefCell::new(None);
    let st = st.clone();
    scrolled.add_tick_callback(move |sw, clock| {
        let t = clock.frame_time();
        let t0 = *start.borrow_mut().get_or_insert(t);
        let adj = sw.vadjustment();
        let target = speed * (t - t0) as f64 / 1e6;
        let max = adj.upper() - adj.page_size();
        adj.set_value(target.min(max));
        if matches!(st.borrow().phase, Phase::Scroll) {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}

/// Schedule an insert (or remove) between frames, like an input event would.
fn arm(st: &Rc<RefCell<State>>, model: &RowModel, opts: &Options, rep: usize, insert: bool) {
    let (st, model, n) = (st.clone(), model.clone(), opts.insert_rows);
    glib::timeout_add_local_once(Duration::from_millis(3), move || {
        let mut s = st.borrow_mut();
        let t0 = Instant::now();
        if insert {
            // Insert under a row in the middle of the list (currently visible).
            let at = model.data().rows.len() / 2;
            s.insert_at = at;
            model.insert_children(at, n, 1000 + rep as u64);
            s.res.insert_splice_ms.push(ms(t0.elapsed()));
            s.phase = Phase::InsertPending { rep, t0 };
        } else {
            model.remove_children(s.insert_at, n);
            s.res.remove_splice_ms.push(ms(t0.elapsed()));
            s.phase = Phase::RemovePending { rep, t0 };
        }
    });
}

/// Close the first window without quitting, then open a fresh one later.
fn reopen(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    model: &RowModel,
    opts: &Options,
) {
    let guard = app.hold();
    let (app, window, model, opts) = (app.clone(), window.clone(), model.clone(), opts.clone());
    glib::idle_add_local_once(move || {
        window.destroy();
        // Let the compositor and GTK settle, as if the user closed the window
        // a while ago.
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            let _guard = guard;
            REOPEN_T0.with(|c| c.set(Some(Instant::now())));
            crate::build_window(&app, &opts, &model, Some(Mode::Reopened));
        });
    });
}

fn finish(r: &Results, window: &adw::ApplicationWindow, opts: &Options, mode: Mode) {
    let renderer = window
        .renderer()
        .map(|r| r.type_().name().to_string())
        .unwrap_or_default();
    let refresh_mhz = window
        .surface()
        .and_then(|s| gdk::Display::default().and_then(|d| d.monitor_at_surface(&s)))
        .map(|m| m.refresh_rate())
        .unwrap_or(0);
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"variant\":\"{}\",\"mode\":\"{mode:?}\",\"rows\":{},\"renderer\":\"{renderer}\",\
         \"refresh_mhz\":{refresh_mhz},\"gtk\":\"{}.{}.{}\",\"startup_ms\":{:.3},\
         \"first_paint_epoch_ns\":{}",
        opts.variant.name(),
        opts.rows,
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        r.startup_ms,
        r.first_paint_epoch_ns,
    );
    out.push_str(",\"marks_ms\":{");
    crate::MARKS.with(|m| {
        let m = m.borrow();
        for (i, (k, v)) in m.iter().enumerate() {
            let _ = write!(out, "{}\"{k}\":{v:.1}", if i > 0 { "," } else { "" });
        }
    });
    out.push('}');
    if let Some(v) = REOPEN_MS.with(|c| c.get()) {
        let _ = write!(out, ",\"reopen_ms\":{v:.3}");
    }
    if mode == Mode::All {
        let _ = write!(
            out,
            ",\"scroll_speed_px_s\":{},\"insert_rows\":{},",
            opts.speed, opts.insert_rows
        );
        stats("frame_interval_ms", &r.intervals_ms, &mut out);
        out.push(',');
        stats("frame_work_ms", &r.work_ms, &mut out);
        let over = r.intervals_ms.iter().filter(|&&x| x > 16.7).count();
        // A missed vsync at 60 Hz shows up as an interval of ~33 ms.
        let dropped = r.intervals_ms.iter().filter(|&&x| x > 25.0).count();
        let slow_work = r.work_ms.iter().filter(|&&x| x > 16.7).count();
        let _ = write!(
            out,
            ",\"intervals_over_16_7ms\":{over},\"dropped_frames_over_25ms\":{dropped},\
             \"work_over_16_7ms\":{slow_work},"
        );
        stats("insert_splice_ms", &r.insert_splice_ms, &mut out);
        out.push(',');
        stats("insert_to_paint_ms", &r.insert_to_paint_ms, &mut out);
        out.push(',');
        stats("remove_splice_ms", &r.remove_splice_ms, &mut out);
        out.push(',');
        stats("remove_to_paint_ms", &r.remove_to_paint_ms, &mut out);
    }
    out.push('}');
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{out}");
}
