// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Class-template specializations as parameter types. Validates
// the `?$Name@<targs>@` template-segment format (with trailing
// `@` closing the args block) and that distinct template
// instantiations are tracked separately by both back-ref tables.

template<typename T>
struct Box {
    T value;
};

void take_int_box(Box<int>) {}
void take_two(Box<int>, Box<int>) {}
void take_int_and_float(Box<int>, Box<float>) {}
