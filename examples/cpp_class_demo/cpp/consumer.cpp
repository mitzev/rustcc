// C++ consumer that exercises the Rust-authored Counter by calling
// the actual methods, not just asserting layouts. With
// emit-forwarders enabled, `Counter::Counter()`, `Counter::bump(i64)`,
// `Counter::get()`, and `Counter::~Counter()` resolve to Itanium-
// mangled symbols that the rustcc driver routes to real Rust bodies
// via extern-C forwarder thunks.
//
// The binary constructs a Counter, bumps it, reads the value, and
// exits with the read value as the exit code. An exit code of 42
// proves the Rust body actually executed (no abort-ing stub would
// return a meaningful value).

#include "cpp_class_demo-cxx.hpp"

#include <cstdio>
#include <cstdint>

int main() {
    // Stack-allocate and default-construct. C1 ctor via forwarder.
    Counter c;
    // Walk through state transitions. Each call lands in a real
    // Rust method body.
    c.bump(10);
    c.bump(32);
    std::int64_t got = c.get();
    std::fprintf(stdout, "counter=%lld\n", static_cast<long long>(got));
    // Return value doubles as an exit-code assertion the outer
    // integration test reads.
    return static_cast<int>(got);
    // ~Counter() fires here as `c` goes out of scope — routes
    // through the dtor forwarder, which `drop_in_place`s the Rust
    // value.
}
