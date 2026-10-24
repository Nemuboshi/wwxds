/* detection.c includes VSHelper.h for VSMIN/VSMAX. This is the same two-macro
 * helper the frozen benchmark uses (benchmarks/wwxd/VSHelper.h); duplicated so
 * the oracle builds without reaching into the benchmark directory. */
#define VSMAX(a, b) ((a) > (b) ? (a) : (b))
#define VSMIN(a, b) ((a) < (b) ? (a) : (b))
