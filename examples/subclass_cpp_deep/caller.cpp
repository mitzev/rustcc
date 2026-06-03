// Drives the deep-chain subclass from the C++ side. `make_widget` returns
// a Rust-defined `MyWidget` (subclass of the imported `Widget`). We view
// it through every base pointer in the chain, dispatch the virtual each
// level introduced, then `delete` through the GRANDPARENT `Shape*` to
// prove the virtual destructor + Rust `Drop` fire across the full chain.
#include "cpp/cppbase.hpp"
#include <cstdio>

int g_derived_drop = 0;
extern "C" void note_derived_drop() { g_derived_drop++; }

// C++-side layout shadow matching the Rust MyWidget (base + extra int).
struct MyWidget : Widget { virtual int only_mine(); int extra; };

extern "C" MyWidget* make_widget(int x, int extra);

extern "C" int run() {
    MyWidget* w = make_widget(7, 5);
    Shape*    sh = static_cast<Shape*>(w);     // grandparent (2 levels up)
    Drawable* dr = static_cast<Drawable*>(w);  // parent
    Widget*   wi = static_cast<Widget*>(w);    // direct base

    int a = sh->area();      // via Shape*    -> Rust override = 1005
    int z = dr->z_order();   // via Drawable* -> Rust override = 2005
    int h = wi->handle();    // via Widget*   -> Rust override = 3005
    int m = w->only_mine();  // MyWidget-only virtual = 5

    delete sh;               // delete via the GRANDPARENT Shape* (virtual dtor)

    if (a != 1005) { printf("FAIL area=%d\n", a); return 1; }
    if (z != 2005) { printf("FAIL z_order=%d\n", z); return 2; }
    if (h != 3005) { printf("FAIL handle=%d\n", h); return 3; }
    if (m != 5)    { printf("FAIL only_mine=%d\n", m); return 4; }
    if (g_shape_ctor != 1)   { printf("FAIL shape_ctor=%d\n", g_shape_ctor); return 5; }
    if (g_shape_dtor != 1)   { printf("FAIL shape_dtor=%d\n", g_shape_dtor); return 6; }
    if (g_derived_drop != 1) { printf("FAIL derived_drop=%d\n", g_derived_drop); return 7; }

    printf("area=%d z_order=%d handle=%d only_mine=%d | "
           "shape_ctor=%d shape_dtor=%d derived_drop=%d "
           "-- DEEP 3-level subclass + virtual dtor cross-boundary OK\n",
           a, z, h, m, g_shape_ctor, g_shape_dtor, g_derived_drop);
    return 0;
}
