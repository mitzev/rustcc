// @kind layout
// @target x86_64-pc-windows-msvc
//
// Multiple-inheritance layouts. Validates:
// - Non-polymorphic MI: each base contributes its full nv_size in
//   declaration order, no overlap.
// - Polymorphic MI: each polymorphic base gets its own vptr; the
//   primary base shares the derived class's offset 0, the
//   secondary base starts after the primary's full size.

struct A { int a; };
struct B { int b; };
struct C : A, B { int c; };
C _c;

struct VA { virtual void f() {} int x; };
struct VB { virtual void g() {} int y; };
struct VC : VA, VB { int z; };
VC _vc;
