
//! Member-function-pointer round-trips (v1.14 phase 1).
//!
//! 1. C++-produced member pointers — non-virtual, VIRTUAL (its
//!    encoding rides the adj low bit on AArch64), and null — pass
//!    through Rust as opaque `CxxMemberFnPtr<Receiver>` values and
//!    dispatch correctly when handed back.
//! 2. Rust CONSTRUCTS `{fn_addr, 0}` targeting a Rust function with
//!    the member ABI (`this` first) and C++ invokes through it —
//!    the wx `Connect`-shaped capability.

include!("../target/gen-out/bindings.rs");

use cxx::CxxMemberFnPtr;

/// Member-ABI handler: Itanium member functions are free functions
/// with `this` prepended, so a plain `extern "C" fn(this, args…)`
/// matches on AArch64/x86-64.
extern "C" fn rust_member(this: *mut Receiver, v: i32) -> i32 {
    let _ = this; // receiver arrives in the `this` slot
    1000 + v
}

#[unsafe(no_mangle)]
pub extern "C" fn run_test() -> i32 {
    unsafe {
        let mut r = Receiver::new(10);
        let rp = &mut r as *mut Receiver;

        // 1a. Non-virtual member pointer round-trip: add -> 10 + 7.
        let f_add = Hooks::get_add();
        if Hooks::invoke(rp, f_add, 7) != 17 {
            println!("FAIL nonvirtual add");
            return 1;
        }
        // 1b. VIRTUAL member pointer round-trip: vadd -> 10*10 + 7.
        let f_vadd = Hooks::get_vadd();
        if Hooks::invoke(rp, f_vadd, 7) != 107 {
            println!("FAIL virtual vadd");
            return 2;
        }
        // 1c. Null round-trip, both directions.
        let f_null = Hooks::get_null();
        if !f_null.is_null() || !Hooks::is_null(f_null) {
            println!("FAIL null");
            return 3;
        }
        if Hooks::is_null(f_add) || f_add.is_null() {
            println!("FAIL non-null misread as null");
            return 4;
        }
        // 2. Rust-constructed member pointer, invoked BY C++:
        //    {rust_member as usize, 0} -> (r->*f)(7) == 1007.
        let f_rust: CxxMemberFnPtr<Receiver> =
            CxxMemberFnPtr::from_nonvirtual_fn(rust_member as usize);
        if Hooks::invoke(rp, f_rust, 7) != 1007 {
            println!("FAIL rust-constructed member fn");
            return 5;
        }
        println!(
            "add=17 vadd=107 null=ok rust_member=1007 -- MEMBER FN POINTERS OK"
        );
        0
    }
}
