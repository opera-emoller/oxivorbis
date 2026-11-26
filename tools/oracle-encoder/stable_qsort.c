/*
 * Stable, signature-compatible qsort replacement for the oracle build.
 * See stable_qsort.h for why. Bottom-up merge sort with a heap scratch
 * buffer; on ties the element from the left run wins, so equal elements
 * keep their input order.
 */
#include <stdlib.h>
#include <string.h>

void lw_stable_qsort(void *base, size_t nmemb, size_t size,
                     int (*compar)(const void *, const void *)) {
    if (nmemb < 2 || size == 0) return;

    unsigned char *a = base;
    unsigned char *tmp = malloc(nmemb * size);
    if (!tmp) abort();

    unsigned char *src = a, *dst = tmp;
    for (size_t width = 1; width < nmemb; width *= 2) {
        for (size_t lo = 0; lo < nmemb; lo += 2 * width) {
            size_t mid = lo + width < nmemb ? lo + width : nmemb;
            size_t hi = lo + 2 * width < nmemb ? lo + 2 * width : nmemb;
            size_t i = lo, j = mid, k = lo;
            while (i < mid && j < hi) {
                /* Take from the right run only if strictly smaller. */
                if (compar(src + j * size, src + i * size) < 0) {
                    memcpy(dst + k * size, src + j * size, size);
                    j++;
                } else {
                    memcpy(dst + k * size, src + i * size, size);
                    i++;
                }
                k++;
            }
            if (i < mid) memcpy(dst + k * size, src + i * size, (mid - i) * size);
            else if (j < hi) memcpy(dst + k * size, src + j * size, (hi - j) * size);
        }
        unsigned char *t = src;
        src = dst;
        dst = t;
    }
    if (src != a) memcpy(a, src, nmemb * size);
    free(tmp);
}
