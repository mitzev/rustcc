// The fork emits Itanium C++ typeinfo nodes (`_ZTI*`, `_ZTV*`)
// that reference `__cxxabiv1::__class_type_info` and
// `__si_class_type_info`. Those symbols live in libc++abi on
// macOS / libstdc++ or libc++abi on Linux. Link the C++ standard
// library so the linker resolves them; that also pulls in the
// C++ personality for exception handling, which Itanium dtors
// expect even if we never throw.

fn main() {
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=stdc++");
    }
}
