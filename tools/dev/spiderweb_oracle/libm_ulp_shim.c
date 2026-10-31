// Usage (Linux, glibc): gcc -O2 -shared -fPIC -o /tmp/libulp.so libm_ulp_shim.c -ldl -lm
//   then: cargo test -p plumspirit_engine --test v08_spiderweb --no-run, and run the test
//   binary with LD_PRELOAD=/tmp/libulp.so. The strict (reference) path must fail only
//   with C-math differences; see docs/BUILTIN_PLUGINS.md "Oracle platform".
// LD_PRELOAD shim: every call of these libm functions returns its result moved
// by one ulp (a stand-in for another platform's C math library).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <math.h>
static double nudge(double r){ return (isfinite(r) && r != 0.0) ? nextafter(r, INFINITY) : r; }
#define F1(n) double n(double x){ static double(*f)(double); if(!f) f=dlsym(RTLD_NEXT,#n); return nudge(f(x)); }
#define F2(n) double n(double x,double y){ static double(*f)(double,double); if(!f) f=dlsym(RTLD_NEXT,#n); return nudge(f(x,y)); }
F1(sin) F1(cos) F1(tan) F1(exp) F1(log) F1(log2) F1(log10) F1(acos) F1(asin) F1(atan)
F2(pow) F2(atan2) F2(hypot)
