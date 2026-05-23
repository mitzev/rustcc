// Implementations for the demo library declared in `library.hpp`.
// The matching throws shims (`__rustcc_throws_*`) live in a
// generated `cxx_shims.cpp` that the build.rs orchestrator
// produces under `OUT_DIR`.

#include "library.hpp"
#include <stdexcept>

int add(int a, int b) {
    return a + b;
}

int do_divide(int a, int b) {
    if (b == 0) {
        throw std::runtime_error("divide by zero");
    }
    return a / b;
}

int compute(int selector) {
    switch (selector) {
        case 0: return 100;
        case 1: throw DomainError("bad domain");
        case 2: throw RangeError("out of range");
        case 3: throw std::runtime_error("plain std exception");
        default: throw 42;  // catch (...)
    }
}

Calc::Calc(int seed) : seed_(seed) {
    if (seed < 0) {
        throw DomainError("negative seed");
    }
}

int Calc::read() const {
    return seed_;
}

int Calc::divide(int divisor) {
    if (divisor == 0) {
        throw std::runtime_error("calc divide by zero");
    }
    return seed_ / divisor;
}
