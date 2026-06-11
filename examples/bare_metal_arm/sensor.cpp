#include "sensor.hpp"

// GCC emits Sensor's vtable + typeinfo here (read() is the key
// function), so the Rust-emitted derived RTTI chains to a
// GCC-compiled _ZTI6Sensor — exactly the cross-compiler seam the
// probe exists to exercise.
Sensor::Sensor(int32_t i) : id(i) {}
int32_t Sensor::read() const { return id * 10; }
int32_t Sensor::unit() const { return 42; }
