#[path = "../../nebula_app/build/i18n.rs"]
mod i18n;

fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let output = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    i18n::generate(&root.join("../../nebula_app/i18n"), &output.join("translations.rs")).unwrap();
}
