// This probe declares `#[cpp_virtual]` methods (via the `virtual` /
// `override` class modifiers), so the binary emits Itanium RTTI that
// references the C++ runtime's `__cxxabiv1::__class_type_info`
// vtables. Link the platform C++ runtime so the link step resolves
// them. (The non-virtual probes don't need this.)
fn main() {
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
    // MSVC links the C++ runtime implicitly; nothing to add.
}
