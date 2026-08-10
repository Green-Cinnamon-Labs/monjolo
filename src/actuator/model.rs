/** actuator/model.rs

Implementação concreta de `Actuator` (o trait de `actuator/mod.rs`) — bloco genérico e reaproveitável
do framework: um atuador de estado único com dinâmica de 1ª ordem arbitrária. Não presume a fórmula
— quem constrói fornece a lei física via closure; `monjolo` só cuida da mecânica comum (comando,
estado, derivada, inscrição no StateRegistry, `write()`/`evaluate()`).

Um único tipo serve pra qualquer atuador de qualquer planta: `Actuator::new(registry, key,
dynamics)`, uma instância por atuador físico, diferindo só pela chave e pela closure. "Ser uma
válvula de alimentação D" não é mais uma questão de tipo Rust — é só a chave e a closure passadas
nesta instância.
*/

use std::cell::Cell;
use std::rc::Rc;

use crate::dynamic_model::DynamicModel;
use crate::state_registry::{Proxy, StateRegistry};

/** Atuador com um único estado (`state`) e sua derivada, comandado de fora via `write()`. `dynamics`
é a única parte que o framework não conhece: dado o comando atual e o estado atual, devolve dx/dt —
de primeira ordem, de segunda ordem, com saturação, o que for; `Actuator` não presume nada sobre a
forma da lei, só que ela existe.

`command` é `Cell<f64>`, não campo simples: `write(&self, ...)` (contrato de `Actuator`) e
`evaluate(&self)` (contrato de `DynamicModel`) não recebem `&mut self`, mas precisam mutar `command`
— mutabilidade interior, mesmo raciocínio de `EvaluationState`/`Proxy`.
*/
pub struct Actuator {
    command: Cell<f64>,
    state: Proxy,
    derivative: Proxy,
    dynamics: Box<dyn Fn(f64, f64) -> f64>,
}

impl Actuator {
    /** `key` é a chave publicada no StateRegistry pro estado próprio (posição, velocidade, o que
    for); a derivada é sempre publicada como `"{key}.derivative"` — nunca precisa ser redeclarada.
    `dynamics(comando, estado) -> dx/dt` é a lei física, chamada uma vez por `evaluate()`.

    Devolve `Rc<Self>`, não `Self`: "criado = já oferecido" é invariante do tipo — `new()` já
    registra o atuador no catálogo de `StateRegistry` sob a própria `key` (`offer_actuator()`,
    `pub(crate)`) antes de devolver, e devolve o mesmo `Rc` que guardou lá. Esse mesmo `Rc` também é
    o que `add_dynamic` recebe (`Rc<T>: DynamicModel`) — a mesma instância participa da física e
    fica endereçável por nome, sem cópia.
    */
    pub fn new(
        registry: &mut StateRegistry,
        key: &str,
        dynamics: impl Fn(f64, f64) -> f64 + 'static,
    ) -> Rc<Self> {
        let derivative_key = format!("{key}.derivative");
        let (offered, _) = registry.subscribe(&[key, &derivative_key], &[]);

        let actuator = Rc::new(Self {
            command: Cell::new(0.0),
            state: offered[0].clone(),
            derivative: offered[1].clone(),
            dynamics: Box::new(dynamics),
        });
        registry.offer_actuator(key, actuator.clone());
        actuator
    }
}

impl super::Actuator for Actuator {
    fn write(&self, value: f64) {
        self.command.set(value);
    }
}

impl DynamicModel for Actuator {
    fn evaluate(&self) {
        let command = self.command.get();
        let state = self.state.get();
        self.derivative.set((self.dynamics)(command, state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actuator::Actuator as ActuatorTrait;

    #[test]
    fn derivative_is_real_not_a_stub() {
        let registry = StateRegistry::shared();
        let actuator = Actuator::new(&mut registry.borrow_mut(), "feed_a", |command, state| {
            (command - state) / 2.0
        });
        registry.borrow_mut().resolve().unwrap();

        actuator.write(50.0);
        /* estado nasce em 0.0 (default do slot) — derivada esperada: (50-0)/2 = 25 */
        actuator.evaluate();
        assert_eq!(actuator.derivative.get(), 25.0);
    }

    /** Prova que a lei não é presumida — uma dinâmica não-linear qualquer funciona igual, sem
    nenhuma mudança no tipo `Actuator`.
    */
    #[test]
    fn dynamics_law_is_not_hardcoded() {
        let registry = StateRegistry::shared();
        let actuator = Actuator::new(&mut registry.borrow_mut(), "purge", |command, state| {
            (command - state).powi(2).copysign(command - state)
        });
        registry.borrow_mut().resolve().unwrap();

        actuator.write(3.0);
        actuator.evaluate();
        assert_eq!(actuator.derivative.get(), 9.0);
    }

    /** Prova a invariante nova: `Actuator::new()` já registra o atuador no catálogo de
    `StateRegistry`, sob a própria `key` — ninguém precisa chamar `offer_actuator()` à parte, e nem
    poderia (é `pub(crate)`).
    */
    #[test]
    fn new_registers_itself_under_its_own_key() {
        let registry = StateRegistry::shared();
        let actuator = Actuator::new(&mut registry.borrow_mut(), "purge", |command, state| {
            command - state
        });
        let actuator: Rc<dyn ActuatorTrait> = actuator;

        let found = registry.borrow().actuator("purge").expect("deveria estar no catálogo");
        assert!(Rc::ptr_eq(&actuator, &found));
    }
}
