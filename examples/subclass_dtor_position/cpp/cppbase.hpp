// The dtor is NOT the first virtual — the shape the old fork mis-built
// ([D1,D0,early,late] vs clang's [early,D1,D0,late]).
struct NotFirst {
    int x;
    explicit NotFirst(int x_);
    virtual int early();
    virtual ~NotFirst();
    virtual int late();
};
extern int g_ctor, g_dtor;
