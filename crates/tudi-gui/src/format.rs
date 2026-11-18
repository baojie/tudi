//! Display formatting: sizes, dates, and icons guessed from file names only.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::gio;
use gtk::glib;

/// Decimal units, as GNOME shows them.
pub fn size(n: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1000.0;
    let mut u = 0;
    while v >= 999.95 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

/// Modified time: `HH:MM` today, `Mon DD` this year, else `YYYY-MM-DD`.
pub fn date(secs: i64, now: &glib::DateTime) -> String {
    let Ok(t) = glib::DateTime::from_unix_local(secs) else {
        return String::new();
    };
    if t.ymd() == now.ymd() {
        t.format("%H:%M").map(|s| s.to_string()).unwrap_or_default()
    } else if t.year() == now.year() {
        if crate::i18n::is_english() {
            // %b follows the system locale; keep English UIs English.
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            format!("{} {}", MONTHS[(t.month() - 1) as usize], t.day_of_month())
        } else {
            t.format("%b %-d")
                .map(|s| s.to_string())
                .unwrap_or_default()
        }
    } else {
        t.format("%Y-%m-%d")
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

/// Lossy display form of a file name.
pub fn name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

thread_local! {
    static ICONS: RefCell<HashMap<Vec<u8>, gio::Icon>> = RefCell::new(HashMap::new());
}

/// Symbolic icon for a file, guessed from its name and cached per extension.
/// Symbolic icons avoid decoding colour icons through glycin (ADR-012).
pub fn file_icon(name: &[u8]) -> gio::Icon {
    let ext = match name.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => name[p..].to_ascii_lowercase(),
        _ => Vec::new(),
    };
    ICONS.with(|cache| {
        cache
            .borrow_mut()
            .entry(ext)
            .or_insert_with_key(|ext| {
                let probe = format!("x{}", String::from_utf8_lossy(ext));
                let (ctype, _) = gio::content_type_guess(Some(probe.as_str()), None);
                with_fallback(gio::content_type_get_symbolic_icon(&ctype))
            })
            .clone()
    })
}

/// Icon themes do not all have the specific symbolic icons GIO names (e.g.
/// `application-octet-stream-symbolic`); end the fallback chain with a
/// generic document icon that every theme has, instead of "missing image".
fn with_fallback(icon: gio::Icon) -> gio::Icon {
    match icon.downcast::<gio::ThemedIcon>() {
        Ok(t) => {
            t.append_name("text-x-generic-symbolic");
            t.upcast()
        }
        Err(icon) => icon,
    }
}

/// Icon for an executable regular file (known once metadata arrived).
pub fn executable_icon() -> gio::Icon {
    gio::ThemedIcon::from_names(&[
        "application-x-executable-symbolic",
        "system-run-symbolic",
        "text-x-generic-symbolic",
    ])
    .upcast()
}

pub fn folder_icon() -> gio::Icon {
    gio::ThemedIcon::new("folder-symbolic").upcast()
}

use gtk::prelude::Cast;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1000), "1.0 kB");
        assert_eq!(size(88_000), "88 kB");
        assert_eq!(size(2_100_000), "2.1 MB");
        assert_eq!(size(999_960), "1.0 MB");
    }
}
