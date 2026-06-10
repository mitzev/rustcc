#include "receiver.hpp"
Receiver::Receiver(int b) : base(b) {}
int Receiver::add(int v) { return base + v; }
int Receiver::vadd(int v) { return base * 10 + v; }
AddFn Hooks::get_add() { return &Receiver::add; }
AddFn Hooks::get_vadd() { return &Receiver::vadd; }
AddFn Hooks::get_null() { return nullptr; }
int Hooks::invoke(Receiver* r, AddFn f, int v) { return (r->*f)(v); }
bool Hooks::is_null(AddFn f) { return f == nullptr; }
