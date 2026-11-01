//! Gives `libmmreg.so` an ELF SONAME (`libmmreg.so.<major>`) so distributions can ship the
//! versioned library in the runtime package and keep the `libmmreg.so` symlink in `-dev`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(os.as_str(), "linux" | "android") {
        let major = std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap_or_default();
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libmmreg.so.{major}");
    }
}
