# Zephyr C++ interop probe — rustcc `class` under Zephyr RTOS.
#
# A Zephyr app whose `main` runs the four cross-compiler dispatch
# checks from examples/bare_metal_arm (Rust `class`, Rust subclass via
# a base pointer, Rust subclass of an IMPORTED g++-compiled base —
# override + inherited slot), proving the fork's vtables / ctor vptr
# install / cross-compiler RTTI work under Zephyr's build + runtime,
# not just the hand-rolled FreeRTOS one.
#
# The Rust side is the *same* validated crate as the FreeRTOS probe
# (examples/bare_metal_arm), built into libbare_metal_arm.a by
# run_zephyr.sh and passed in via -DRUSTCC_RUST_LIB. The C++ side
# (caller/sensor/rtti_stub) is reused byte-for-byte too.

cmake_minimum_required(VERSION 3.20.0)
find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(rustcc_zephyr_cpp C CXX)

set(BMA ${CMAKE_CURRENT_SOURCE_DIR}/../bare_metal_arm)

target_sources(app PRIVATE
  src/main.c
  ${BMA}/caller.cpp     # demo() entry points + placement-new + virtual calls
  ${BMA}/sensor.cpp     # the imported g++-compiled C++ base
  ${BMA}/rtti_stub.c    # __cxxabiv1 type_info vtables for the RTTI chain
)
target_include_directories(app PRIVATE ${BMA})

# Zephyr compiles C++ with -fno-rtti, but the imported base's typeinfo
# (`_ZTI6Sensor`) must exist for the fork-emitted `_ZTI6Reader` RTTI
# chain to resolve — so the TU that defines Sensor's vtable emits it
# with RTTI on (exactly as the FreeRTOS probe compiles sensor.cpp).
# The __si_class_type_info vtable it references comes from rtti_stub.c.
set_source_files_properties(${BMA}/sensor.cpp PROPERTIES COMPILE_OPTIONS "-frtti")

# Prebuilt Rust `class` staticlib (built for the Cortex-M3 target by
# run_zephyr.sh). caller.cpp references its init_*/ctor/vtable symbols,
# so the linker pulls the class metadata in.
if(NOT DEFINED RUSTCC_RUST_LIB)
  message(FATAL_ERROR "pass -DRUSTCC_RUST_LIB=/abs/path/libbare_metal_arm.a (see run_zephyr.sh)")
endif()
target_link_libraries(app PRIVATE ${RUSTCC_RUST_LIB})
