// @kind mangle
//
// Non-type template parameters (NTTPs) and template-template
// arguments, Itanium ABI. Validates the `L<type><number>E`
// literal encoding for integral arguments (`Li4E`, `Lm4E`, `Lb1E`,
// `Lc65E`, negative `Lin1E`) and the bare-name form for
// template-template arguments (`3Box`).

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
