#pragma once
// v1.14 phase 1 testbed: C++ pointer-to-member-function crossing the
// boundary in BOTH directions. `AddFn` values produced by C++ travel
// through Rust opaquely (incl. a VIRTUAL member pointer, whose
// ARM-variant adj-discriminator Rust must preserve); Rust also
// CONSTRUCTS a non-virtual member pointer targeting a Rust function
// and C++ invokes through it — the wx `Connect(&Class::OnEvent)` shape.
struct Receiver {
    int base;
    explicit Receiver(int b);
    int add(int v);            // non-virtual: base + v
    virtual int vadd(int v);   // virtual:     base * 10 + v
};
typedef int (Receiver::*AddFn)(int);
struct Hooks {
    int dummy;
    static AddFn get_add();
    static AddFn get_vadd();
    static AddFn get_null();
    static int invoke(Receiver* r, AddFn f, int v);  // (r->*f)(v)
    static bool is_null(AddFn f);
};
