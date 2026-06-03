#include "cpp/cppbase.hpp"
#include <cstdio>

// Mirror MyWidget's C++ shape so we can also dispatch its new virtual
// through a MyWidget*. (The Rust side owns the real definition.)
struct MyWidget : CppBase {
    virtual int only_mine();   // new virtual at slot 2
    int extra;
};

extern "C" MyWidget* make_widget(int x, int extra);
extern "C" void free_widget(MyWidget* w);

extern "C" int run() {
    MyWidget* w = make_widget(7, 5);
    CppBase* base = static_cast<CppBase*>(w);

    int f = base->foo();        // expect MyWidget::foo      = 5+100 = 105
    int d = base->describe();   // expect MyWidget::describe = 5+200 = 205 (pure overridden!)
    int m = w->only_mine();     // expect MyWidget::only_mine = 5*3 = 15

    free_widget(w);

    if (f != 105) { std::printf("FAIL foo via CppBase*=%d expected 105\n", f); return 1; }
    if (d != 205) { std::printf("FAIL describe via CppBase*=%d expected 205\n", d); return 2; }
    if (m != 15)  { std::printf("FAIL only_mine via MyWidget*=%d expected 15\n", m); return 3; }

    std::printf(
        "foo-via-CppBase* = %d (concrete override); "
        "describe-via-CppBase* = %d (PURE override); "
        "only_mine-via-MyWidget* = %d (new virtual) -- imported-base cross-boundary OK\n",
        f, d, m);
    return 0;
}
