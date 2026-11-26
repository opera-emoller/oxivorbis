/*
 * Hand-written replacement for libogg's configure-generated
 * <ogg/config_types.h> (config_types.h.in in the libogg 1.3.5 tarball).
 * Used only by the oracle builds, which compile the vendored libogg
 * sources in tools/libogg-1.3.5/ directly instead of linking a system
 * libogg. The fixed-width <stdint.h> types are what configure/cmake
 * selects on every supported host.
 */
#ifndef __CONFIG_TYPES_H__
#define __CONFIG_TYPES_H__

#include <stdint.h>

typedef int16_t ogg_int16_t;
typedef uint16_t ogg_uint16_t;
typedef int32_t ogg_int32_t;
typedef uint32_t ogg_uint32_t;
typedef int64_t ogg_int64_t;
typedef uint64_t ogg_uint64_t;

#endif
