#include "cppbase.hpp"
int g_ctor = 0; int g_dtor = 0;
NotFirst::NotFirst(int x_) : x(x_) { g_ctor++; }
NotFirst::~NotFirst() { g_dtor++; }
int NotFirst::early() { return -1; }
int NotFirst::late() { return -2; }
