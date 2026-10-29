//! Generates the Swift (or Kotlin) bindings: `cargo run -p indigo-ffi --bin uniffi-bindgen -- --help`.
fn main() {
    uniffi::uniffi_bindgen_main()
}
