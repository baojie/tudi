//! Print what each given folder lives on.
//!
//!     cargo run -p tudi-index --example fsinfo -- /home /extra

fn main() {
    for a in std::env::args().skip(1) {
        let i = tudi_index::fsinfo::detect(std::path::Path::new(&a));
        eprintln!("{a}: {i:?} slow={}", i.slow());
    }
}
