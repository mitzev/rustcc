// @kind vtable
// @target Abstract
//
// Pure virtual methods install `__cxa_pure_virtual` in their slots.
// An abstract class still has `offset_to_top` + RTTI + a D1/D0 pair
// for its (defined) destructor.

struct Abstract {
    virtual ~Abstract();
    virtual int compute() const = 0;
    virtual void render() = 0;
};

Abstract::~Abstract() = default;
