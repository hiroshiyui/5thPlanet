/*
 * 5thPlanet homebrew test disc — entry point (Phase 0 template).
 *
 * Built with libyaul (MIT). Boots on the real-BIOS path, runs the feature
 * checks, and posts a machine-readable result to High Work RAM for the headless
 * harness `crates/saturn/tests/homebrew_disc.rs`. See tests/disc/README.md for
 * the result protocol and the reserved-address caveat.
 *
 * The result-protocol writes are plain volatile stores (toolchain/SDK
 * agnostic). Phase 0 simply reports PASS so the whole pipeline — toolchain →
 * disc → boot → result read — can be proven before any real check exists. Add
 * checks in run_tests() and give each a unique non-zero failure id.
 */

#include <stdint.h>

/* Result block in High Work RAM (HWRAM, 0x06000000..0x060FFFFF). Keep in sync
 * with homebrew_disc.rs AND ensure it sits outside the program/stack/heap
 * (confirm via build/saturn-tests.map). */
#define RESULT_STATUS (*(volatile uint32_t *)0x0603FF04u)
#define RESULT_DETAIL (*(volatile uint32_t *)0x0603FF08u)
#define RESULT_SIG    (*(volatile uint32_t *)0x0603FF00u)
#define SIG_TST1      0x54535431u /* "TST1" */

/* A check returns 0 on pass, or its non-zero test id on failure. Set
 * `*detail` to anything useful for diagnosis (an observed value, an address). */
typedef uint32_t (*test_fn)(uint32_t *detail);

/* Phase 0 has no real checks yet — a single always-pass placeholder proves the
 * harness. Replace/extend with real feature tests (VDP1/VDP2/SCSP/timing),
 * driven by doc/emulation-capabilities-evaluation.md. */
static uint32_t test_smoke(uint32_t *detail) {
    *detail = 0x600D600Du; /* "GOOD GOOD" sentinel — visible in the harness log */
    return 0;
}

static const test_fn TESTS[] = {
    test_smoke,
    /* test_vdp2_priority, test_scsp_envelope, ... */
};
#define NUM_TESTS (sizeof(TESTS) / sizeof(TESTS[0]))

/* Run every check; stop at the first failure and report its id. */
static void run_tests(void) {
    uint32_t detail = 0;
    for (uint32_t i = 0; i < NUM_TESTS; i++) {
        uint32_t id = TESTS[i](&detail);
        if (id != 0) {
            RESULT_DETAIL = detail;
            RESULT_STATUS = id; /* non-zero = failing test id */
            RESULT_SIG = SIG_TST1; /* signature LAST */
            return;
        }
    }
    RESULT_DETAIL = detail;
    RESULT_STATUS = 0; /* all passed */
    RESULT_SIG = SIG_TST1; /* signature LAST */
}

int main(void) {
    run_tests();
    /* Spin so the BIOS doesn't reclaim control; the harness samples the result
     * word and stops once the signature appears. */
    for (;;) {
    }
    return 0;
}
