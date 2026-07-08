// sensor/dynamic.rs
//
// Sensors with dynamic state implementing DynamicModel.
// Stateless sensors (FI, LI, TI, PI, AI) remain in sensor/model.rs.
// These types model sensors whose output has its own response lag —
// e.g., a thermocouple or pressure transmitter with first-order dynamics.

// use crate::dynamic_model::DynamicModel;

// ── FirstOrderSensor ──────────────────────────────────────────────────────────
// Models a sensor with first-order response lag: d(output)/dt = (input - output) / tau
// State: [output]  (one variable: current sensor output)
//
// Useful for: thermocouples, slow pressure transmitters, any sensor
// where the reading lags the physical value.

pub struct FirstOrderSensor {
    pub tau: f64,
    pub input: f64,
}

impl FirstOrderSensor {
    pub fn new(tau: f64) -> Self {
        Self { tau, input: 0.0 }
    }

    pub fn set_input(&mut self, physical_value: f64) {
        self.input = physical_value;
    }

    pub fn output(state: &[f64]) -> f64 {
        state[0]
    }
}

// impl DynamicModel for FirstOrderSensor {
//     fn state_size(&self) -> usize {
//         1
//     }
//
//     fn dynamics(&mut self, state: &[f64]) -> Vec<f64> {
//         let output = state[0];
//         vec![(self.input - output) / self.tau]
//     }
//
//     fn name(&self) -> &'static str {
//         "FirstOrderSensor"
//     }
// }

// ── SampledSensor ─────────────────────────────────────────────────────────────
// Models a sampler/analyzer with transport delay.
// State: [held_value]  (one variable: the last sampled value, held until next sample)
//
// The held value is updated externally at each sample event;
// between events the derivative is zero (zero-order hold).

pub struct SampledSensor {
    pub input: f64,
    pub sample_pending: bool,
}

impl SampledSensor {
    pub fn new() -> Self {
        Self {
            input: 0.0,
            sample_pending: false,
        }
    }

    pub fn trigger(&mut self, physical_value: f64) {
        self.input = physical_value;
        self.sample_pending = true;
    }

    pub fn output(state: &[f64]) -> f64 {
        state[0]
    }
}

// impl DynamicModel for SampledSensor {
//     fn state_size(&self) -> usize {
//         1
//     }
//
//     fn dynamics(&mut self, state: &[f64]) -> Vec<f64> {
//         // Zero-order hold: output is constant between samples.
//         // The sample event updates input externally via trigger().
//         let held = state[0];
//         if self.sample_pending {
//             vec![(self.input - held) / 1e-6] // fast step toward new sample
//         } else {
//             vec![0.0]
//         }
//     }
//
//     fn name(&self) -> &'static str {
//         "SampledSensor"
//     }
// }
