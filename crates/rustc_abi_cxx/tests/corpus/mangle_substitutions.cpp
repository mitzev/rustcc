// @kind mangle
//
// Substitution-table sequencing. Each previously-seen "substitutable
// entity" (a compound type, not a builtin) may be referenced by
// `S_`, `S0_`, `S1_`, ... in later positions. This file forces long
// chains by reusing types in multi-argument signatures.

struct A {};
struct B {};
struct C {};

// First A is spelled out (and becomes S_); second A uses S_; B becomes
// S0_; second B uses S0_; C becomes S1_; later A uses S_ again.
void mix(A a1, A a2, B b1, B b2, C c, A a3) {
    (void)a1; (void)a2; (void)b1; (void)b2; (void)c; (void)a3;
}

// References and pointers create their own substitutable entities
// (`RK A` is substitutable as a distinct compound type from `A`).
void refs(const A& x, const A& y, A* p, A* q) {
    (void)x; (void)y; (void)p; (void)q;
}
