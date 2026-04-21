// @target Widget
//
// Polymorphic class with a vptr at offset 0. The first data field `value`
// is pushed to offset 8 (64-bit pointer width). Alignment is raised to 8
// by the pointer. dsize=12 (end of `value`); sizeof=16 with tail padding.

struct Widget {
    virtual ~Widget();
    virtual void render();
    int value;
};

Widget::~Widget() = default;
void Widget::render() {}
