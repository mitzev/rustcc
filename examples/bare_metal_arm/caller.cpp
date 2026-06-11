// This file is intended to be cross-compiled for ARM Cortex-M
// (e.g. thumbv7em-none-eabihf) via arm-none-eabi-g++. An IDE
// analyzing it under the host toolchain may flag the typedefs
// below — that is expected; they match the ARM32 ABI.

typedef int int32_t;
typedef __SIZE_TYPE__ size_t;  // compiler built-in — correct per target

// Inline placement-new declaration to avoid needing <new> from a
// (potentially absent) libstdc++ headers on a bare-metal toolchain.
inline void* operator new(size_t, void* p) noexcept { return p; }

struct Widget {
    // const matches Rust's `fn foo(&self)` — mangles `_ZNK…`
    // (v1.13.10 signature-carrying slots).
    virtual int32_t foo() const;
    int32_t v;

    Widget(int32_t v);   // Rust-defined ctor
};

alignas(4) unsigned char storage[8];

extern "C" int demo(int v) {
    Widget* w = new (storage) Widget(v);   // references _ZN6WidgetC1Ei
    return (int)w->foo();                  // virtual dispatch through vtable
}

// Rust factory: constructs a DERIVED Rust class (Gauge : Widget) in
// static storage and returns it upcast to the base. g++ cannot see
// the dynamic type, so the call below is a genuine indirect dispatch
// through the vtable — it must land in Rust's `override fn foo`.
extern "C" Widget* init_gauge(int32_t v, int32_t scale);

extern "C" int demo_subclass(int v, int scale) {
    Widget* w = init_gauge(v, scale);
    return (int)w->foo();                  // expects the Gauge override
}

// Imported-base flavor: Sensor is a real C++ class (sensor.cpp, GCC-
// compiled); Rust subclasses it heap-free and hands back a Sensor*.
#include "sensor.hpp"

extern "C" Sensor* init_reader(int32_t id, int32_t offset);

extern "C" int demo_imported_override(int id, int off) {
    return (int)init_reader(id, off)->read();   // Rust override
}
extern "C" int demo_imported_base(int id, int off) {
    return (int)init_reader(id, off)->unit();   // inherited C++ impl
}
