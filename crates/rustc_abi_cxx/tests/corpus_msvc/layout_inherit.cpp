// @kind layout
// @target x86_64-pc-windows-msvc
//
// Single-inheritance layouts under MSVC. Validates:
// - Base subobject placed at offset 0.
// - MSVC does NOT reuse tail padding of a non-POD base (size 12 not 8).
// - Polymorphic class has vptr at offset 0.
// - Derived from a polymorphic base inherits the vptr (no extra vptr).

struct Base {
    int x;
    char y;
};
Base _b;

struct Derived : Base {
    char z;
};
Derived _d;

struct Poly {
    virtual void f() {}
    int x;
};
Poly _p;

struct PolyDerived : Poly {
    int y;
};
PolyDerived _pd;

struct EmptyBase {};
struct WithEmptyBase : EmptyBase {
    int x;
};
WithEmptyBase _w;
