// The polymorphic classes in this probe emit Itanium RTTI that
// references the C++ runtime's `__cxxabiv1` typeinfo vtables; link the
// platform C++ runtime so the link step resolves them.
fn main() {
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}
