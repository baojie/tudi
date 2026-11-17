//! gettext wrappers. UI strings are English in code; translations live in
//! `po/*.po` and are compiled to `.mo` by `build.rs`.
//!
//! Placeholders are `{}` (positional) and replaced with [`fill`].

use gettextrs::{bind_textdomain_codeset, bindtextdomain, gettext, ngettext, textdomain};

pub const DOMAIN: &str = "tudi";

/// Bind the text domain: the installed locale dir, or (for a build run
/// from the source tree) the catalogs compiled by `build.rs`.
pub fn init() {
    let dir = std::env::var_os("TUDI_LOCALEDIR")
        .map(std::path::PathBuf::from)
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
