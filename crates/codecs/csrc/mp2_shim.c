/* Thin wrapper around minimp3 (CC0) used for DAB MPEG-1/2 Layer II audio. */
#define MINIMP3_IMPLEMENTATION
#define MINIMP3_FLOAT_OUTPUT
#include <stdlib.h>
#include "minimp3.h"

void *magicdab_mp2_new(void)
{
    mp3dec_t *d = (mp3dec_t *)malloc(sizeof(mp3dec_t));
    if (d)
        mp3dec_init(d);
    return d;
}

void magicdab_mp2_free(void *d)
{
    free(d);
}

/* Decode one frame. Returns number of samples per channel, fills channels / sample rate / frame bytes. */
int magicdab_mp2_decode(void *d, const unsigned char *data, int len, float *pcm, int *channels, int *hz, int *frame_bytes, int *layer)
{
    mp3dec_frame_info_t info;
    int samples = mp3dec_decode_frame((mp3dec_t *)d, data, len, pcm, &info);
    *channels = info.channels;
    *hz = info.hz;
    *frame_bytes = info.frame_bytes;
    *layer = info.layer;
    return samples;
}
