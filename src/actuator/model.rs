// actuator/model.rs

pub trait Actuator {
    fn apply(&mut self, control_signal: f64, dt: f64) -> f64;
}

pub struct IdealActuator;

impl Actuator for IdealActuator {
    fn apply(&mut self, control_signal: f64, _dt: f64) -> f64 {
        control_signal
    }
}
