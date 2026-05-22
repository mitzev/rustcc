// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Nested namespace + class mangling under MSVC.
//
// Covers:
// - Nested namespace `N::M`.
// - Class `N::M::S` (struct, mangled with `U`).
// - Back-reference compression when the same scope is reused.

namespace N {
    namespace M {
        struct S { int x; };
        int f(S s) { return s.x; }
    }
    int g(M::S s) { return s.x; }
}
