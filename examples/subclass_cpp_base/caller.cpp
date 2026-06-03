#include "cpp/cppbase.hpp"
#include <cstdio>

// Rust `Drop`-ran counter, incremented from the Rust MyWidget::drop.
int g_derived_drop = 0;
extern "C" void note_derived_drop() { g_derived_drop++; }

// Mirror MyWidget's C++ shape so we can also dispatch its new virtual
// through a MyWidget*. (The Rust side owns the real definition.)
struct MyWidget : CppBase {
    virtual int only_mine();   // new virtual at slot 4 (after D1,D0,foo,describe)
    int extra;
};

extern "C" MyWidget* make_widget(int x, int extra);

extern "C" int run() {
    MyWidget* w = make_widget(7, 5);
    CppBase* base = static_cast<CppBase*>(w);

    int f = base->foo();        // expect MyWidget::foo      = 5+100 = 105
    int d = base->describe();   // expect MyWidget::describe = 5+200 = 205 (pure overridden)
    int m = w->only_mine();     // expect MyWidget::only_mine = 5*3 = 15

    // Destroy through the base pointer: the vtable's deleting destructor
    // runs the Rust MyWidget::drop, destroys the CppBase subobject, and
    // frees — each exactly once.
    delete base;

    if (f != 105) { std::printf("FAIL foo via CppBase*=%d expected 105\n", f); return 1; }
    if (d != 205) { std::printf("FAIL describe via CppBase*=%d expected 205\n", d); return 2; }
    if (m != 15)  { std::printf("FAIL only_mine via MyWidget*=%d expected 15\n", m); return 3; }
    if (g_base_ctor != 1) { std::printf("FAIL base_ctor=%d expected 1\n", g_base_ctor); return 4; }
    if (g_base_dtor != 1) { std::printf("FAIL base_dtor=%d expected 1 (base subobject not destroyed once)\n", g_base_dtor); return 5; }
    if (g_derived_drop != 1) { std::printf("FAIL derived_drop=%d expected 1 (Rust Drop not run once)\n", g_derived_drop); return 6; }

    std::printf(
        "foo=%d describe=%d only_mine=%d | base_ctor=%d base_dtor=%d derived_drop=%d "
        "-- imported-base subclass + virtual-dtor cross-boundary OK\n",
        f, d, m, g_base_ctor, g_base_dtor, g_derived_drop);
    return 0;
}
