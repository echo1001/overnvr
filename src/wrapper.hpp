#include <nvbufsurface.h>
#include <nvbufsurftransform.h>
#include "cuda_runtime.h"
#include "cudaEGL.h"
#include <nvdsinfer_context.h>

typedef void* surface_encoded;

extern "C" {
    int surface_encode(NvBufSurface *surf, surface_encoded *encoded);
    unsigned long surface_len(surface_encoded encoded);
    unsigned char* surface_data(surface_encoded encoded);
    void surface_free(surface_encoded encoded);

}