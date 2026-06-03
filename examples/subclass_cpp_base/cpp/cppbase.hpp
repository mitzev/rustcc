#pragma once

// An imported C++ polymorphic base with a VIRTUAL DESTRUCTOR, one
// concrete virtual (`foo`), and one pure virtual (`describe`). A Rust
// `class MyWidget : CppBase` subclasses it; `delete (CppBase*)widget`
// from C++ runs the Rust `Drop`, destroys the base subobject, and frees
// — the canonical "C++ owns a Rust-defined polymorphic object" shape
// (e.g. an FLTK widget owned by its parent group).
//
// Uses plain `int` (no `<cstdint>`) so the header parses standalone
// under libclang without a configured C++ sysroot.
struct CppBase {
    int x;

    explicit CppBase(int x_);
    virtual ~CppBase();              // VIRTUAL destructor

    virtual int foo();               // concrete  -> vtable slot 2 (after D1,D0)
    virtual int describe() = 0;      // pure      -> vtable slot 3

    int base_x() const;              // non-virtual helper
};

// Test instrumentation: balanced ctor/dtor counts prove no leak / no
// double-free across the boundary.
extern int g_base_ctor;
extern int g_base_dtor;
