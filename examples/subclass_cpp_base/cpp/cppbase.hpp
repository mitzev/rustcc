#pragma once

// An imported C++ polymorphic base with one *concrete* virtual (`foo`)
// and one *pure* virtual (`describe`). The destructor is non-virtual
// (in scope for this landing — a virtual destructor across the boundary
// is future work). `foo` is the key function (first non-inline,
// non-pure virtual defined out-of-line), so clang anchors `_ZTV7CppBase`
// / `_ZTI7CppBase` in cppbase.o.
//
// Uses plain `int` (no `<cstdint>`) so the header parses standalone
// under libclang without a configured C++ sysroot.
struct CppBase {
    int x;

    explicit CppBase(int x_);

    virtual int foo();           // concrete  -> vtable slot 0
    virtual int describe() = 0;  // pure      -> vtable slot 1

    int base_x() const;          // non-virtual helper
};
