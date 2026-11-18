//! Display formatting: sizes and dates.

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
