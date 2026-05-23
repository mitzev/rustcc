// Tiny throwing C++ function for the Phase 2 native-invoke
// integration test. Returns int32_t by value; throws on
// negative input.

#include <stdexcept>
#include <cstdint>

extern "C" int32_t maybe_throws(int32_t x) {
    if (x < 0) {
        throw std::runtime_error("negative input");
    }
    return x * 2;
}
