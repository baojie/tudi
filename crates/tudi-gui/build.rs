//! Compile `po/*.po` into `$OUT_DIR/locale/<lang>/LC_MESSAGES/tudi.mo` with
//! `msgfmt`, so a binary run from the build tree finds its translations.
//! If `msgfmt` is missing, the UI is simply untranslated.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let po_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../po");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("locale");
    println!("cargo:rerun-if-changed={}", po_dir.display());
    println!("cargo:rustc-env=TUDI_BUILD_LOCALEDIR={}", out.display());
    let Ok(entries) = std::fs::read_dir(&po_dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "po") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let lang = path.file_stem().unwrap().to_string_lossy().into_owned();
        let dir = out.join(&lang).join("LC_MESSAGES");
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("msgfmt")
            .arg("--check")
            .arg("-o")
            .arg(dir.join("tudi.mo"))
            .arg(&path)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => panic!("msgfmt failed for {}: {s}", path.display()),
            Err(e) => println!("cargo:warning=msgfmt not available ({e}); UI untranslated"),
        }
    }
}
