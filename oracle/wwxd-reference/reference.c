/* Reference-vector oracle: the pinned WWXD control, exposed for a Rust driver.
 *
 * bench_wwxd_create / bench_wwxd_process / bench_wwxd_free reproduce the
 * reference harness behaviour field for field (same MBParam layout, same
 * padding, same fcode = 4, same first-frame marker and shot-age update), so the
 * reference decision and internal state match the pinned control. This is an
 * independent implementation, not a wrapper around the VapourSynth plugin, and
 * it is the sole C control the Rust port is checked against. If the pinned
 * detection core changes, the build.rs commit gate fails and this file must be
 * reviewed.
 *
 * bench_wwxd_digest_predictors / bench_wwxd_digest_previous are read-only helpers
 * that FNV-1a hash the reused macroblock array and the full padded previous plane
 * in the same field/byte order as Rust Detector::state_digests, so parity pins
 * every predictor, sad16, and mode, and the padding replication, not only the
 * boolean decision. FNV-1a uses the standard offset basis and prime.
 */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "detection.h"

#define EDGE 64

#define FNV_OFFSET 0xcbf29ce484222325ULL
#define FNV_PRIME  0x00000100000001b3ULL

typedef struct {
    MBParam param;
    uint8_t *previous, *current;
    MACROBLOCK *blocks;
    int first, intra_count;
    size_t plane_bytes;
} BenchWwxd;

void bench_wwxd_free(BenchWwxd *state) {
    if (!state) return;
    free(state->previous);
    free(state->current);
    free(state->blocks);
    free(state);
}

BenchWwxd *bench_wwxd_create(int width, int height) {
    if (width < 48 || height < 48 || width > 4096 || height > 4096 ||
        (uint64_t)width * (uint64_t)height > 2097152) return NULL;
    BenchWwxd *state = calloc(1, sizeof(*state));
    if (!state) return NULL;
    state->param = (MBParam){
        (uint32_t)width, (uint32_t)height,
        (uint32_t)((width + 15) / 16 * 16 + 128),
        (uint32_t)((height + 15) / 16 * 16 + 128),
        (uint32_t)((width + 15) / 16),
        (uint32_t)((height + 15) / 16), EDGE
    };
    state->plane_bytes = (size_t)state->param.edged_width *
        (state->param.edged_height + 1) + 64;
    state->previous = calloc(state->plane_bytes, 1);
    state->current = calloc(state->plane_bytes, 1);
    state->blocks = calloc((size_t)state->param.mb_width * state->param.mb_height,
        sizeof(*state->blocks));
    if (!state->previous || !state->current || !state->blocks) {
        bench_wwxd_free(state);
        return NULL;
    }
    state->first = 1;
    state->intra_count = 1;
    return state;
}

int bench_wwxd_process(BenchWwxd *state, const uint8_t *luma, int stride) {
    if (!state || !luma || stride < (int)state->param.width) return -1;
    const MBParam *p = &state->param;
    uint8_t *current = state->current + EDGE * p->edged_width + EDGE;
    const uint8_t *previous = state->previous + EDGE * p->edged_width + EDGE;
    uint32_t padded_width = p->mb_width * 16;
    for (uint32_t y = 0; y < p->height; ++y) {
        uint8_t *row = current + (size_t)y * p->edged_width;
        memcpy(row, luma + (size_t)y * stride, p->width);
        memset(row + p->width, row[p->width - 1], padded_width - p->width);
    }
    for (uint32_t y = p->height; y < p->mb_height * 16; ++y)
        memcpy(current + (size_t)y * p->edged_width,
            current + (size_t)(p->height - 1) * p->edged_width, p->edged_width);
    int cut = state->first ? 1 :
        MEanalysis(previous, current, p, state->blocks, state->intra_count, 4);
    state->first = 0;
    if (cut) state->intra_count = 1;
    else if (state->intra_count < INT32_MAX) ++state->intra_count;
    uint8_t *swap = state->previous;
    state->previous = state->current;
    state->current = swap;
    return cut;
}

int bench_wwxd_shot_age(const BenchWwxd *state) { return state->intra_count; }

static void fnv_bytes(uint64_t *hash, const uint8_t *data, size_t length) {
    for (size_t i = 0; i < length; ++i) {
        *hash ^= data[i];
        *hash *= FNV_PRIME;
    }
}

static void fnv_i32(uint64_t *hash, int32_t value) {
    uint8_t bytes[4];
    memcpy(bytes, &value, 4); /* native little-endian, matching Rust to_le_bytes */
    fnv_bytes(hash, bytes, 4);
}

uint64_t bench_wwxd_digest_previous(const BenchWwxd *state) {
    uint64_t hash = FNV_OFFSET;
    fnv_bytes(&hash, state->previous, state->plane_bytes);
    return hash;
}

uint64_t bench_wwxd_digest_predictors(const BenchWwxd *state) {
    uint64_t hash = FNV_OFFSET;
    size_t count = (size_t)state->param.mb_width * state->param.mb_height;
    for (size_t i = 0; i < count; ++i) {
        const MACROBLOCK *mb = &state->blocks[i];
        for (int q = 0; q < 4; ++q) {
            fnv_i32(&hash, mb->mvs[q].x);
            fnv_i32(&hash, mb->mvs[q].y);
        }
        fnv_i32(&hash, mb->sad16);
        fnv_i32(&hash, (int32_t)mb->mode);
    }
    return hash;
}
