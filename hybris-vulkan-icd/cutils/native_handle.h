#ifndef NATIVE_HANDLE_H_
#define NATIVE_HANDLE_H_
#include <stdint.h>
typedef struct native_handle { int version, numFds, numInts; int data[0]; } native_handle_t;
#endif
