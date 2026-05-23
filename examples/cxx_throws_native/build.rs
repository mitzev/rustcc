// Compile + link the throwing C++ source against the
// Rust crate. cc::Build handles platform-specific exception
// flags (-fexceptions on Itanium-ABI hosts).

fn main() {
    cc::Build::new()
        .cpp(true)
        .file("cpp/maybe_throws.cpp")
        .flag_if_supported("-fexceptions")
        .compile("maybe_throws");

    // Link against the C++ standard library (libc++ on
    // macOS/Linux clang, libstdc++ on g++).
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=stdc++");
    }
    // Windows MSVC: vcruntime is auto-linked.
}
