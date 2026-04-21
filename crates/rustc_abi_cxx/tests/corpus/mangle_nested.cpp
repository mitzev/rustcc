// @kind mangle
//
// Nested-name mangling: namespace scoping produces `N<scopes>E` wrappers.
// Substitutions should kick in for repeated references to the enclosing
// scope, e.g. when the parameter type names the enclosing class.

namespace outer {
namespace inner {

struct Bar {
    void baz();
    void self(const Bar& other);
    Bar();
    ~Bar();
};

}  // namespace inner
}  // namespace outer

void outer::inner::Bar::baz() {}
void outer::inner::Bar::self(const Bar& other) { (void)other; }
outer::inner::Bar::Bar() {}
outer::inner::Bar::~Bar() {}
