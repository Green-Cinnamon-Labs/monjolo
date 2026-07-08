// sensor/model.rs

use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand_distr::{Distribution, Normal};

pub trait Sensor {
    fn measure(&mut self, physical_value: f64, dt: f64) -> f64;
    fn noise_std(&self) -> f64;
}

// ── IdealSensor ───────────────────────────────────────────────────────────────

pub struct IdealSensor;

impl Sensor for IdealSensor {
    fn measure(&mut self, physical_value: f64, _dt: f64) -> f64 {
        physical_value
    }
    fn noise_std(&self) -> f64 {
        0.0
    }
}

// ── Shared noise helper ───────────────────────────────────────────────────────

fn apply_noise(value: f64, std_dev: f64, rng: &mut SmallRng) -> f64 {
    if std_dev == 0.0 {
        return value;
    }
    let dist = Normal::new(0.0, std_dev).expect("invalid noise_std");
    value + dist.sample(rng)
}

// ── FI — Flow Indicator ───────────────────────────────────────────────────────

pub struct FI {
    noise_std: f64,
    rng: SmallRng,
}

impl FI {
    pub fn new(noise_std: f64, seed: u64) -> Self {
        Self { noise_std, rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Sensor for FI {
    fn measure(&mut self, physical_value: f64, _dt: f64) -> f64 {
        apply_noise(physical_value, self.noise_std, &mut self.rng)
    }
    fn noise_std(&self) -> f64 {
        self.noise_std
    }
}

// ── PI — Pressure Indicator ───────────────────────────────────────────────────

pub struct PI {
    noise_std: f64,
    rng: SmallRng,
}

impl PI {
    pub fn new(noise_std: f64, seed: u64) -> Self {
        Self { noise_std, rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Sensor for PI {
    fn measure(&mut self, physical_value: f64, _dt: f64) -> f64 {
        apply_noise(physical_value, self.noise_std, &mut self.rng)
    }
    fn noise_std(&self) -> f64 {
        self.noise_std
    }
}

// ── LI — Level Indicator ──────────────────────────────────────────────────────

pub struct LI {
    noise_std: f64,
    rng: SmallRng,
}

impl LI {
    pub fn new(noise_std: f64, seed: u64) -> Self {
        Self { noise_std, rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Sensor for LI {
    fn measure(&mut self, physical_value: f64, _dt: f64) -> f64 {
        apply_noise(physical_value, self.noise_std, &mut self.rng)
    }
    fn noise_std(&self) -> f64 {
        self.noise_std
    }
}

// ── TI — Temperature Indicator ────────────────────────────────────────────────

pub struct TI {
    noise_std: f64,
    rng: SmallRng,
}

impl TI {
    pub fn new(noise_std: f64, seed: u64) -> Self {
        Self { noise_std, rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Sensor for TI {
    fn measure(&mut self, physical_value: f64, _dt: f64) -> f64 {
        apply_noise(physical_value, self.noise_std, &mut self.rng)
    }
    fn noise_std(&self) -> f64 {
        self.noise_std
    }
}

// ── AI — Analyzer Indicator ───────────────────────────────────────────────────
// Composition analyzers (XMEAS 23-41): sampled at fixed intervals.
// The sampling gate is managed by the model; AI applies noise when called.

pub struct AI {
    noise_std: f64,
    rng: SmallRng,
}

impl AI {
    pub fn new(noise_std: f64, seed: u64) -> Self {
        Self { noise_std, rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Sensor for AI {
    fn measure(&mut self, physical_value: f64, _dt: f64) -> f64 {
        apply_noise(physical_value, self.noise_std, &mut self.rng)
    }
    fn noise_std(&self) -> f64 {
        self.noise_std
    }
}
