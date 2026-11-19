//! gettext wrappers. UI strings are English in code; translations live in
//! `po/*.po` and are compiled to `.mo` by `build.rs`.
//!
//! Placeholders are `{}` (positional) and replaced with [`fill`].

use gettextrs::{bind_textdomain_codeset, bindtextdomain, gettext, ngettext, textdomain};

pub const DOMAIN: &str = "tudi";

thread_local! {
    static ENGLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Whether the UI is in English (the default, `[ui] language = "en"`).
pub fn is_english() -> bool {
    ENGLISH.with(|e| e.get())
}

/// GTK stack libraries whose own strings (dialogs, `--help`) would
/// otherwise follow the system language.
const STACK_DOMAINS: &[&str] = &["glib20", "gtk40", "libadwaita"];

/// Set up translations for `language`. English: tudi's strings are used as
/// written, and the GTK stack's catalogs are pointed at a directory without
/// catalogs so they stay English too. System: bind tudi's catalogs (see
/// [`bind`]) and let the locale decide. A specific language: the same, with
/// `LANGUAGE` set to it, which gettext consults before the locale (also for
/// GTK's own strings). Call before any thread is started.
pub fn init(language: tudi_core::config::Language) {
    use tudi_core::config::Language;
    match language {
        Language::Locale(code) => {
            // SAFETY: called at the start of `main`, before GTK or any
            // other thread exists, so nothing reads the environment
            // concurrently.
            unsafe { std::env::set_var("LANGUAGE", &code) };
            ENGLISH.with(|e| e.set(false));
            bind();
        }
        Language::En => {
            ENGLISH.with(|e| e.set(true));
            for d in STACK_DOMAINS {
                let _ = bindtextdomain(*d, "/nonexistent/tudi-english");
            }
            let _ = textdomain(DOMAIN);
        }
        Language::System => {
            ENGLISH.with(|e| e.set(false));
            bind();
        }
    }
}

/// Bind the text domain. Looks in `$TUDI_LOCALEDIR`, then
/// `<prefix>/share/locale` for a binary in `<prefix>/bin` (e.g. a
/// `~/.local` install), then the catalogs compiled by `build.rs` (a build
/// run from the source tree), then `/usr/share/locale`.
fn bind() {
    let installed = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?.parent()?.join("share/locale");
        dir.join("zh_CN/LC_MESSAGES/tudi.mo")
            .is_file()
            .then_some(dir)
    });
    let dir = std::env::var_os("TUDI_LOCALEDIR")
        .map(std::path::PathBuf::from)
        .or(installed)
        .or_else(|| {
            let built = std::path::PathBuf::from(env!("TUDI_BUILD_LOCALEDIR"));
            built.is_dir().then_some(built)
        })
        .unwrap_or_else(|| "/usr/share/locale".into());
    // GTK calls setlocale() during its initialization. Binding failures
    // only mean untranslated strings.
    let _ = bindtextdomain(DOMAIN, dir);
    let _ = bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = textdomain(DOMAIN);
}

/// Translate.
pub fn tr(msgid: &str) -> String {
    gettext(msgid)
}

/// Translate with a plural form chosen by `n`.
pub fn ntr(singular: &str, plural: &str, n: u64) -> String {
    ngettext(singular, plural, u32::try_from(n).unwrap_or(u32::MAX))
}

/// Replace each `{}` in `template` with the next argument.
pub fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(p) = rest.find("{}") {
        out.push_str(&rest[..p]);
        match args.next() {
            Some(a) => out.push_str(&a.to_string()),
            None => out.push_str("{}"),
        }
        rest = &rest[p + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn fills_in_order() {
        assert_eq!(fill("{} of {}", &[&1, &"b"]), "1 of b");
        assert_eq!(fill("no args", &[]), "no args");
        assert_eq!(fill("{} {}", &[&"x"]), "x {}");
    }
}
