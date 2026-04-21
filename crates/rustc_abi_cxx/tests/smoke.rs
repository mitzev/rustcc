use rustc_abi_cxx::{CxxTypeCtx, Target};

#[test]
fn ctx_constructs_on_each_target() {
    let _ = CxxTypeCtx::new(Target::x86_64_unknown_linux_gnu());
    let _ = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let _ = CxxTypeCtx::new(Target::aarch64_unknown_linux_gnu());
    let _ = CxxTypeCtx::new(Target::aarch64_apple_darwin());
}
