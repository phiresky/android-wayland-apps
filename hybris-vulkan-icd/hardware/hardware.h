#ifndef ANDROID_HARDWARE_H
#define ANDROID_HARDWARE_H
#include <stdint.h>
#define MAKE_TAG_CONSTANT(A,B,C,D) (((A) << 24) | ((B) << 16) | ((C) << 8) | (D))
#define HARDWARE_MODULE_TAG MAKE_TAG_CONSTANT('H','W','M','T')
#define HARDWARE_MAKE_API_VERSION(maj,min) ((((maj) & 0xff) << 8) | ((min) & 0xff))
struct hw_module_t {
    uint32_t tag; uint16_t module_api_version; uint16_t hal_api_version;
    const char *id, *name, *author; void *methods, *dso;
    uint8_t reserved[sizeof(void*) == 8 ? 32-7*sizeof(void*) : 4];
};
struct hw_device_t {
    uint32_t tag, version; struct hw_module_t *module;
    uint8_t reserved[12]; int (*close)(struct hw_device_t *device);
};
int hw_get_module(const char *id, const struct hw_module_t **module);
#endif
