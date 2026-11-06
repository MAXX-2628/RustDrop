//! Generates Kotlin bindings: `cargo run -p uniffi-bindgen -- generate --library <lib.so> --language kotlin --out-dir <dir>`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
