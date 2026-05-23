// C++ source for the typed-catch smoke test. Two distinct
// std::exception subclasses so we can verify the landingpad
// dispatches based on typeinfo.

#include <stdexcept>
#include <cstdint>

class DomainError : public std::runtime_error {
public:
    DomainError() : std::runtime_error("domain") {}
};

class RangeError : public std::runtime_error {
public:
    RangeError() : std::runtime_error("range") {}
};

extern "C" int32_t maybe_throws_typed(int32_t x) {
    if (x == -1) {
        throw DomainError{};
    }
    if (x == -2) {
        throw RangeError{};
    }
    return x * 2;
}
