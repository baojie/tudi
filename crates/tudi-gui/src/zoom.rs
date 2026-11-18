//! Interface zoom (`Ctrl+=`, `Ctrl+-`, `Ctrl+0`): one app-wide CSS
//! provider scales the font size of tudi windows and the size of row icons.
//! GTK lays out again from CSS, so rows grow with the text.

use std::cell::{Cell, RefCell};

use gtk::gdk;

/// Zoom steps in percent.
pub const MIN: u32 = 70;
pub const MAX: u32 = 200;
pub const STEP: u32 = 10;
/// Symbolic icon size at 100 %.
const ICON_PX: u32 = 16;
/// File icon size at 100 % (drawn icons read better a little larger).
const FILE_ICON_PX: u32 = 18;

thread_local! {
    static LEVEL: Cell<u32> = const { Cell::new(100) };
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

pub fn level() -> u32 {
    LEVEL.with(|l| l.get())
}

fn css(level: u32) -> String {
    let icon = (ICON_PX * level).div_ceil(100);
    let file_icon = (FILE_ICON_PX * level).div_ceil(100);
    // Percent font sizes on the toplevel are relative to the theme's font.
    format!(
        "window.tudi-window {{ font-size: {level}%; }}\n\
         window.tudi-window image.tudi-icon {{ -gtk-icon-size: {icon}px; }}\n\
         window.tudi-window fileicon {{ min-width: {file_icon}px; min-height: {file_icon}px; }}\n"
    )
}

/// Set the zoom level (clamped), app-wide. Returns the level applied.
pub fn set(level: u32) -> u32 {
    let level = level.clamp(MIN, MAX);
    LEVEL.with(|l| l.set(level));
    PROVIDER.with(|p| {
        let mut p = p.borrow_mut();
        let provider = p.get_or_insert_with(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
                );
            }
            provider
        });
        provider.load_from_string(&css(level));
    });
    level
}

pub fn zoom_in() -> u32 {
    set(level() + STEP)
}

pub fn zoom_out() -> u32 {
    set(level().saturating_sub(STEP))
}

pub fn reset() -> u32 {
    set(100)
}

#[cfg(test)]
mod tests {
    use super::css;

    #[test]
    fn css_scales_font_and_icons() {
        assert!(css(100).contains("font-size: 100%") && css(100).contains("16px"));
        assert!(css(150).contains("font-size: 150%") && css(150).contains("24px"));
        assert!(css(130).contains("21px"));
    }
}
