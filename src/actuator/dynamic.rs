// actuator/dynamic.rs

/** Valve/Agitator são os únicos componentes até agora sem nenhum `need` — a
 dinâmica deles não depende de mais nada além do próprio estado (posição/
 velocidade) e do comando recebido de fora via set_command(). Por isso são
 os primeiros com uma derivada REAL (não um TODO): `evaluate()` calcula
 `(command - position) / tau` e escreve tanto o valor lido de volta quanto
 a derivada, ambos via Proxy.
 
 Cada instância precisa de um `name` único (ex.: "feed_a", "agitator") pra
 não colidir no StateRegistry — há várias válvulas na planta (XMV-01..11).
*/
use crate::dynamic_model::DynamicModel;
use crate::state_registry::{Proxy, StateRegistry};

// ── Valve ───────────────────────────────────────────────────────────
// Models a control valve with first-order lag: d(position)/dt = (command - position) / tau
// State: [position]  (one variable: current valve position, 0–100 %)

pub struct Valve {
    tau: f64,
    command: f64,
    position: Proxy,
    derivative: Proxy,
}

impl Valve {
    pub fn new(registry: &mut StateRegistry, name: &str, tau: f64) -> Self {
        let position_key = format!("valve.{name}.position");
        let derivative_key = format!("valve.{name}.position.derivative");
        let (offered, _) = registry.subscribe(&[&position_key, &derivative_key], &[]);

        Self {
            tau,
            command: 0.0,
            position: offered[0].clone(),
            derivative: offered[1].clone(),
        }
    }

    pub fn set_command(&mut self, command: f64) {
        self.command = command;
    }
}

impl DynamicModel for Valve {
    fn name(&self) -> &'static str {
        "Valve"
    }

    fn evaluate(&self) {
        let position = self.position.get();
        self.derivative.set((self.command - position) / self.tau);
    }
}

// ── Agitator ──────────────────────────────────────────────────────────────────
// Reactor agitator speed — same first-order dynamics as a valve but
// controls mixing intensity (agsp), not fluid flow.
// State: [speed]  (one variable: current agitator speed, 0–100 %)

pub struct Agitator {
    tau: f64,
    command: f64,
    speed: Proxy,
    derivative: Proxy,
}

impl Agitator {
    pub fn new(registry: &mut StateRegistry, tau: f64) -> Self {
        let (offered, _) = registry.subscribe(
            &["agitator.speed", "agitator.speed.derivative"],
            &[],
        );

        Self {
            tau,
            command: 0.0,
            speed: offered[0].clone(),
            derivative: offered[1].clone(),
        }
    }

    pub fn set_command(&mut self, command: f64) {
        self.command = command;
    }
}

impl DynamicModel for Agitator {
    fn name(&self) -> &'static str {
        "Agitator"
    }

    fn evaluate(&self) {
        let speed = self.speed.get();
        self.derivative.set((self.command - speed) / self.tau);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valve_derivative_is_real_not_a_stub() {
        let registry = StateRegistry::shared();
        let mut valve = Valve::new(&mut registry.borrow_mut(), "feed_a", 2.0);
        registry.borrow_mut().resolve().unwrap();

        valve.set_command(50.0);
        // posição nasce em 0.0 (default do slot) — derivada esperada: (50-0)/2 = 25
        valve.evaluate();
        assert_eq!(valve.derivative.get(), 25.0);
    }
}
