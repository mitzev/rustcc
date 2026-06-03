#include "cppbase.hpp"

CppBase::CppBase(int x_) : x(x_) {}

int CppBase::foo() { return x * 2; }   // key function

// describe() is pure: deliberately no definition.

int CppBase::base_x() const { return x; }
