// @kind vtable
// @target x86_64-pc-windows-msvc
//
// Simple polymorphic class — three virtual functions including
// destructor. Validates the basic MSVC vftable layout: RTTI
// pointer in the COL slot, then function pointers in declaration
// order.

struct A {
    virtual void f() {}
    virtual int g(int x) { return x; }
    virtual ~A() {}
};
A _a;
