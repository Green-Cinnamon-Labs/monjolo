/** ## State of a single cubic disturbance channel.

 A disturbance is modeled as a piecewise cubic polynomial that is
 continuously regenerated over time, producing a smooth random signal.

 The polynomial for a channel is:
   f(t) = a + h*(b + h*(c + h*d))   where h = t - t_last

 When `t >= t_next`, a new segment is computed by `update_segment`,
 ensuring C1 continuity (value and first derivative match at joints).
*/
#[derive(Debug, Clone)]
pub struct DisturbanceChannel {
    /// Cubic polynomial coefficients for the current segment
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,

    /// Time at which the current segment started
    pub t_last: f64,
    /// Time at which the current segment ends
    pub t_next: f64,

    /// Shape parameters that control segment duration
    pub h_span: f64,
    pub h_zero: f64,

    /// Shape parameters that control signal amplitude
    pub s_span: f64,
    pub s_zero: f64,
    pub sp_span: f64,

    /// Whether this channel is active (0 = off, 1 = on)
    pub active: i32,
}

impl DisturbanceChannel {
    /// Create a new channel with given shape parameters.
    /// The polynomial is initialized flat at `s_zero`.
    pub fn new(h_span: f64, h_zero: f64, s_span: f64, s_zero: f64, sp_span: f64) -> Self {
        Self {
            a: s_zero,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            t_last: 0.0,
            t_next: 0.1,
            h_span,
            h_zero,
            s_span,
            s_zero,
            sp_span,
            active: 0,
        }
    }
}

/**
 State for a collection of cubic disturbance channels plus the shared pseudo-random number generator seed.

 This struct is generic — it holds only the mechanism.
 Callers are responsible for initializing channels with the
 correct shape parameters for their application.

 Equivalent to COMMON /WLK/ + COMMON /RANDSD/ in teprob.f
*/
#[derive(Debug, Clone)]
pub struct CubicDisturbanceState {
    pub channels: Vec<DisturbanceChannel>,
    pub rand_seed: f64,
}

impl CubicDisturbanceState {
    /// Create a new state with pre-built channels and a given seed.
    pub fn new(channels: Vec<DisturbanceChannel>, rand_seed: f64) -> Self {
        Self {
            channels,
            rand_seed,
        }
    }
}

// =============== Core functions (TESUB5–8) ===============

/** ## Evaluate the cubic disturbance polynomial for channel `idx` at time `t`.
 f(t) = a + h*(b + h*(c + h*d))   where h = t - t_last
 Direct translation of TESUB8 from teprob.f
*/
pub fn eval_disturbance(idx: usize, time: f64, state: &CubicDisturbanceState) -> f64 {
    let ch = &state.channels[idx];
    let h = time - ch.t_last;
    ch.a + h * (ch.b + h * (ch.c + h * ch.d))
}

/** ## Update the cubic polynomial segment for channel `idx`.

 Generates a new C1-continuous cubic segment from the current
 position `(s, sp)` to a randomly chosen future point.
 Updates `a`, `b`, `c`, `d`, and `t_next` in place.

 Direct translation of SUBROUTINE TESUB5 from teprob.f
*/
pub fn update_segment(idx: usize, s: f64, sp: f64, state: &mut CubicDisturbanceState) {
    let h = state.channels[idx].h_span * lcg_rand(-1, state) + state.channels[idx].h_zero;
    let active = state.channels[idx].active as f64;
    let s1 = state.channels[idx].s_span * lcg_rand(-1, state) * active + state.channels[idx].s_zero;
    let s1p = state.channels[idx].sp_span * lcg_rand(-1, state) * active;

    let ch = &mut state.channels[idx];
    ch.a = s;
    ch.b = sp;
    ch.c = (3.0 * (s1 - s) - h * (s1p + 2.0 * sp)) / (h * h);
    ch.d = (2.0 * (s - s1) + h * (s1p + sp)) / (h * h * h);
    ch.t_next = ch.t_last + h;
}

/** ## Generate approximate Gaussian white noise with standard deviation `std`.

 Uses the Irwin-Hall method: sum of 12 uniform samples minus 6,
 scaled by `std`. Approximates N(0, std²) by the Central Limit Theorem.

 Direct translation of SUBROUTINE TESUB6 from teprob.f
*/
pub fn white_noise(std: f64, state: &mut CubicDisturbanceState) -> f64 {
    let sum: f64 = (0..12).map(|_| lcg_rand(0, state)).sum();
    (sum - 6.0) * std
}

/** ## Linear congruential pseudo-random number generator.

 Identical to the generator in TESUB7 from teprob.f.
 Mutates `rand_seed` in `state` on every call.

 `i >= 0` → returns value in [0, 1)
 `i <  0` → returns value in [-1, 1)
*/
pub fn lcg_rand(i: i32, state: &mut CubicDisturbanceState) -> f64 {
    const MOD: f64 = 4_294_967_296.0;
    state.rand_seed = (state.rand_seed * 9_228_907.0).rem_euclid(MOD);
    if i >= 0 {
        state.rand_seed / MOD
    } else {
        2.0 * state.rand_seed / MOD - 1.0
    }
}
