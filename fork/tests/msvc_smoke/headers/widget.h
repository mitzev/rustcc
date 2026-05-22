// Smoke-test C++ header for the v1.09.0 MSVC ABI surface.
// Mirrors the kind of class shape that drives `cxx_importer` in
// real consumer crates: a small polymorphic class with virtual
// methods + a virtual dtor + a non-trivial ctor that takes a
// scalar arg.

#pragma once

class Widget {
public:
    Widget(int initial_count);
    virtual ~Widget();

    virtual int next();
    virtual int current() const;

    int count_;
};
