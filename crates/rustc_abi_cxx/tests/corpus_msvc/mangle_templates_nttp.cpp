// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Non-type template parameters (NTTPs) and template-template
// arguments. Validates the `$0<number>` integral encoding (single
// digit `0`-`9` for magnitudes 1..=10, base-16 `A`-`P` nibbles
// otherwise, leading `?` for negatives) and the `U<name>@@`
// struct-tag form used for template-template arguments.

template<class T, int N> struct Arr { T tag; };
template<class T, unsigned long long N> struct SizeArr { T tag; };
template<bool B> struct Flag { int x; };
template<char C> struct CharBox { int x; };
template<class T> struct Box { T v; };
template<class T, template<class> class C> struct Stack { C<T> impl; };

void take_arr4(Arr<int, 4>) {}
void take_arrneg(Arr<int, -1>) {}
void take_sizearr(SizeArr<int, 4>) {}
void take_flag(Flag<true>) {}
void take_charbox(CharBox<'A'>) {}
void take_stack(Stack<int, Box>) {}
