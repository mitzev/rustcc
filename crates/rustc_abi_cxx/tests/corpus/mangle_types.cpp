// @kind mangle
//
// Parameter-type mangling. Covers:
// - Pointer (`P`) and const-pointer (`PK`) qualifiers.
// - Lvalue (`R`) and rvalue (`O`) reference qualifiers.
// - Top-level `const` on a by-value pointer argument is dropped from
//   mangling (C++ ABI rule), so `f_ptr` and `f_ptr_const` both mangle
//   to the same symbol — Clang only emits one.
// - Unscoped (`enum E`) and scoped (`enum class Scoped`) enum types.

struct S {};

void f_ptr(S* p) { (void)p; }
void f_cptr(const S* p) { (void)p; }
void f_ref(S& r) { (void)r; }
void f_cref(const S& r) { (void)r; }
void f_rref(S&& r) { (void)r; }

enum E { A, B };
enum class Scoped { X, Y };

void f_enum(E e) { (void)e; }
void f_scoped(Scoped s) { (void)s; }
