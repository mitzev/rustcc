// C++-side helpers for the advanced editor demo. Two jobs:
//
// 1. DISPATCH PROBES — call virtuals through BASE-class pointers from
//    real C++ code, proving that C++ virtual dispatch on a base
//    pointer lands in the Rust `override fn`s (the whole point of
//    subclassing). `delete` through `Fl_Widget*` exercises the
//    cross-boundary virtual destructor.
//
// 2. CONSTRUCTION SELF-CHECKS — Fl_Text_Display's constructor creates
//    child scrollbars whose `parent_` points at the object under
//    construction. Rust constructors build the value in a temporary
//    and bitwise-move it to its final address (compilers usually elide
//    this, but Rust does not guarantee it), so `rde_children_parent_ok`
//    verifies at runtime that the self-references survived — turning a
//    silent construct-then-move hazard into a loud test failure.
#include <FL/Fl_Text_Editor.H>
#include <FL/fl_draw.H>
#include <FL/Fl_Group.H>

extern "C" {

// --- dispatch probes (virtual calls through base pointers) ---------
int rde_dispatch_handle(Fl_Widget* w, int ev) { return w->handle(ev); }
void rde_dispatch_resize(Fl_Widget* w, int x, int y, int W, int H) {
    w->resize(x, y, W, H);
}
void rde_dispatch_draw(Fl_Widget* w) { w->draw(); }
void rde_delete_widget(Fl_Widget* w) { delete w; }  // virtual dtor

// --- construction self-checks --------------------------------------
void rde_clear_current_group() { Fl_Group::current(0); }
int rde_children_parent_ok(Fl_Group* g) {
    for (int i = 0; i < g->children(); i++) {
        if (g->child(i)->parent() != g) return 0;
    }
    return 1;
}
int rde_child_count(Fl_Group* g) { return g->children(); }

}  // extern "C"

// v1.14 paint probe: FLTK 1.4's fl_color/fl_rectf are header-INLINE
// free functions (graphics-driver dispatch) — no symbol to bind until
// free-function inline-shim routing lands (tracked). Tiny anchors:
extern "C" void rde_color(unsigned c) { fl_color(c); }
extern "C" void rde_rectf(int x, int y, int w, int h) { fl_rectf(x, y, w, h); }
