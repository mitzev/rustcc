#include "cppbase.hpp"

int g_base_ctor = 0;
int g_base_dtor = 0;

CppBase::CppBase(int x_) : x(x_) { g_base_ctor++; }
CppBase::~CppBase() { g_base_dtor++; }   // key function -> anchors _ZTV/_ZTI

int CppBase::foo() { return x * 2; }
// describe() is pure: deliberately no definition.
int CppBase::base_x() const { return x; }
