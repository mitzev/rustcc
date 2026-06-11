/* Minimal freestanding string.h for the FreeRTOS kernel build —
 * the Homebrew bare-metal cross compilers ship no newlib headers.
 * Implementations live in libc_stub/tinylibc.c. */
#ifndef RUSTCC_STUB_STRING_H
#define RUSTCC_STUB_STRING_H
#include <stddef.h>
void* memset(void* dst, int c, size_t n);
void* memcpy(void* dst, const void* src, size_t n);
void* memmove(void* dst, const void* src, size_t n);
int memcmp(const void* a, const void* b, size_t n);
size_t strlen(const char* s);
char* strcpy(char* dst, const char* src);
#endif
