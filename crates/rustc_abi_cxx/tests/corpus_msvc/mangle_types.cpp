// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Pointer / reference / cv / array variations under MSVC.
//
// No system headers (the cross-target lacks them on macOS) — built-in
// types only.

// Note: source uses `char`/`unsigned char` (plain), not `signed char`/
// `unsigned char`, because our IR collapses I8 into a single signed/
// unsigned distinction and `char`/`unsigned char` are the more common
// source forms. MSVC's mangler distinguishes `char` (`D`) from
// `signed char` (`C`); our pick on signed-I8 is `D`.
void t_void() {}
void t_bool(bool) {}
void t_int8(char) {}
void t_uint8(unsigned char) {}
void t_int16(short) {}
void t_uint16(unsigned short) {}
void t_int32(int) {}
void t_uint32(unsigned int) {}
void t_int64(long long) {}
void t_uint64(unsigned long long) {}
void t_f32(float) {}
void t_f64(double) {}

void p_int(int*) {}
void p_const_int(const int*) {}
void p_volatile_int(volatile int*) {}
void p_const_volatile_int(const volatile int*) {}
void p_p_int(int**) {}

void r_int(int&) {}
void r_const_int(const int&) {}
