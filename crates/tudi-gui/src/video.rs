//! The first frame of a video, for the preview pane (ADR-032).
//!
//! A `playbin` (video only; audio is not decoded) with an `appsink` as
//! video sink is set to PAUSED; prerolling decodes exactly one frame, which
//! is taken from the sink as RGB and returned as pixels. Runs on the
//! pane's loader thread; GStreamer decodes on its own threads. No GL, no
//! window involved. A missing decoder or a broken file gives `None`.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use gtk::{gdk_pixbuf, glib};

/// Longest wait for the first frame (a slow network share, a stuck
/// decoder).
const TIMEOUT: Duration = Duration::from_secs(10);

fn init() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| match gst::init() {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("GStreamer unavailable, no video frames: {e}");
            false
        }
    })
}

/// Decode the first frame of `path`, scaled to fit `max_px` (longest side).
pub fn first_frame(path: &Path, max_px: i32) -> Option<crate::thumbs::Pixels> {
    if !init() {
        return None;
    }
    let uri = glib::filename_to_uri(path, None).ok()?;
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst_video::VideoCapsBuilder::new()
                .format(gst_video::VideoFormat::Rgb)
                .pixel_aspect_ratio(gst::Fraction::new(1, 1))
                .build(),
        )
        .sync(false)
        .max_buffers(1)
        .build();
    // Convert and fix the pixel aspect ratio before the sink.
    let bin = gst::Bin::new();
    let convert = gst::ElementFactory::make("videoconvert").build().ok()?;
    let scale = gst::ElementFactory::make("videoscale").build().ok()?;
    bin.add_many([&convert, &scale, sink.upcast_ref()]).ok()?;
    gst::Element::link_many([&convert, &scale, sink.upcast_ref()]).ok()?;
    let pad = convert.static_pad("sink")?;
    bin.add_pad(&gst::GhostPad::with_target(&pad).ok()?).ok()?;

    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", uri.as_str())
        .property("video-sink", &bin)
        .property(
            "audio-sink",
            gst::ElementFactory::make("fakesink").build().ok()?,
        )
        .build()
        .ok()?;
    // Video only: do not decode audio or subtitles.
    playbin.set_property_from_str("flags", "video");

    let frame = preroll(&playbin, &sink);
    let _ = playbin.set_state(gst::State::Null);
    let (data, w, h, stride) = frame?;
    let pb = gdk_pixbuf::Pixbuf::from_bytes(
        &glib::Bytes::from_owned(data),
        gdk_pixbuf::Colorspace::Rgb,
        false,
        8,
        w,
        h,
        stride,
    );
    let pb = if w.max(h) > max_px {
        let k = f64::from(max_px) / f64::from(w.max(h));
        let (sw, sh) = (
            ((f64::from(w) * k).round() as i32).max(1),
            ((f64::from(h) * k).round() as i32).max(1),
        );
        pb.scale_simple(sw, sh, gdk_pixbuf::InterpType::Bilinear)?
    } else {
        pb
    };
    crate::thumbs::Pixels::from_pixbuf(&pb)
}

/// PAUSE `playbin` and take the prerolled frame: (RGB bytes, width,
/// height, stride).
fn preroll(playbin: &gst::Element, sink: &gst_app::AppSink) -> Option<(Vec<u8>, i32, i32, i32)> {
    playbin.set_state(gst::State::Paused).ok()?;
    let bus = playbin.bus()?;
    let ct = gst::ClockTime::from_nseconds(TIMEOUT.as_nanos() as u64);
    loop {
        let msg =
            bus.timed_pop_filtered(ct, &[gst::MessageType::AsyncDone, gst::MessageType::Error])?;
        match msg.view() {
            gst::MessageView::AsyncDone(_) => break,
            gst::MessageView::Error(e) => {
                tracing::debug!("no video frame: {}", e.error());
                return None;
            }
            _ => {}
        }
    }
    let sample = sink.try_pull_preroll(gst::ClockTime::ZERO)?;
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let buffer = sample.buffer()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
    let (w, h) = (frame.width() as i32, frame.height() as i32);
    let stride = frame.plane_stride()[0];
    let data = frame.plane_data(0).ok()?;
    // Keep only what the pixbuf needs (the last row may lack padding).
    let need = (stride * (h - 1)) as usize + (w * 3) as usize;
    Some((data.get(..need)?.to_vec(), w, h, stride))
}

/// Is this file name a video?
pub fn is_video(name: &[u8]) -> bool {
    crate::icons::category(name) == crate::icons::Category::Video
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a test video with GStreamer itself (skipped when an encoder
    /// or muxer is missing).
    fn make(dir: &Path, name: &str, enc: &str) -> Option<std::path::PathBuf> {
        init().then_some(())?;
        let out = dir.join(name);
        let desc = format!(
            "videotestsrc num-buffers=10 pattern=smpte ! video/x-raw,width=320,height=240 ! {enc} ! filesink location={}",
            out.display()
        );
        let p = gst::parse::launch(&desc).ok()?;
        p.set_state(gst::State::Playing).ok()?;
        let bus = p.bus()?;
        let msg = bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(20),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let _ = p.set_state(gst::State::Null);
        matches!(msg?.view(), gst::MessageView::Eos(_)).then_some(out)
    }

    #[test]
    fn first_frames_of_common_formats() {
        let t = tempfile::tempdir().unwrap();
        let mut tried = 0;
        for (name, enc) in [
            ("a.webm", "vp8enc ! webmmux"),
            ("b.mkv", "vp8enc ! matroskamux"),
            ("c.mp4", "x264enc ! mp4mux"),
        ] {
            let Some(path) = make(t.path(), name, enc) else {
                continue;
            };
            tried += 1;
            let px = first_frame(&path, 1600).unwrap_or_else(|| panic!("{name}: no frame"));
            assert_eq!((px.width(), px.height()), (320, 240), "{name}");
            // SMPTE bars: the top-left bar is light grey, not black/green.
            let (r, g, b) = px.rgb_at(10, 10);
            assert!(r > 150 && g > 150 && b > 150, "{name}: {r},{g},{b}");
            // Scaled to the limit, aspect kept.
            let small = first_frame(&path, 160).unwrap();
            assert_eq!((small.width(), small.height()), (160, 120));
        }
        assert!(tried > 0, "no encoder available to make test videos");
    }

    #[test]
    fn broken_or_missing_files_give_none() {
        let t = tempfile::tempdir().unwrap();
        let junk = t.path().join("junk.mp4");
        std::fs::write(&junk, b"not a video at all").unwrap();
        assert!(first_frame(&junk, 1600).is_none());
        assert!(first_frame(&t.path().join("missing.mkv"), 1600).is_none());
    }
}
