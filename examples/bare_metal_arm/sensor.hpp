// An "imported" C++ polymorphic base for the heap-free subclass
// probe. Deliberately: NON-virtual destructor (so no operator
// new/delete is ever needed — bare-metal friendly), two concrete
// virtual methods (one gets overridden in Rust, one is inherited
// and must dispatch into THIS GCC-compiled object file).
#pragma once

typedef int int32_t;

struct Sensor {
    Sensor(int32_t id);
    virtual int32_t read() const;   // overridden by the Rust subclass
    virtual int32_t unit() const;   // inherited — lands back in C++
    int32_t id;
};
