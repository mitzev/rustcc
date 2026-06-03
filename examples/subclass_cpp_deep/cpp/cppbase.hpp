#pragma once
//
// A *deep* (multi-level) single-inheritance C++ chain:
//
//     Shape  ->  Drawable  ->  Widget
//
// Each level introduces a new virtual, and the chain root (`Shape`) has a
// VIRTUAL DESTRUCTOR. `Widget` — the deepest level — is the imported base
// that a Rust `class MyWidget : Widget` subclasses (see src/mywidget.rs).
//
// The point of the demo: the Rust override must reach virtuals introduced
// at *every* level of the chain, including the grandparent `Shape::area`
// two levels up, and `delete (Shape*)widget` from C++ must run the Rust
// `Drop` + the full C++ destructor chain + free — exactly once. This is
// the canonical "C++ owns a deeply-derived Rust polymorphic object" shape
// (e.g. an FLTK custom widget: Fl_Widget -> Fl_Group -> ... owned by its
// parent group).
//
// Uses plain `int` (no <cstdint>) so the header parses standalone under
// libclang without a configured C++ sysroot.
struct Shape {
    int x;
    explicit Shape(int x_);
    virtual ~Shape();             // VIRTUAL destructor at the root
    virtual int area();           // introduced at Shape   (level 0)
};

struct Drawable : Shape {
    explicit Drawable(int x_);
    virtual int z_order();        // introduced at Drawable (level 1)
};

struct Widget : Drawable {
    explicit Widget(int x_);
    virtual int handle();         // introduced at Widget   (level 2)
};

// Test instrumentation: balanced root ctor/dtor counts prove no leak /
// no double-free across the boundary even when deleting through a base
// pointer two levels up.
extern int g_shape_ctor;
extern int g_shape_dtor;
