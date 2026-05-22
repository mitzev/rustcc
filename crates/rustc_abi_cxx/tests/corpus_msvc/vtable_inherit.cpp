// @kind vtable
// @target x86_64-pc-windows-msvc
//
// Single-inheritance vtable: B inherits A, overrides g + dtor,
// adds a new virtual h. Validates that:
// - Inherited unoverridden methods (A::f) appear with the base
//   class's mangled name.
// - Overridden methods (g, dtor) point at the derived class's
//   implementations.
// - The COL pointer (slot 0) names the most-derived class (B).
// - New virtual methods (h) extend at the end.

struct A {
    virtual void f() {}
    virtual int g(int x) { return x; }
    virtual ~A() {}
};

struct B : A {
    int g(int) override;
    virtual void h() {}
    virtual ~B();
};
int B::g(int x) { return x + 1; }
B::~B() {}

A _a;
B _b;
