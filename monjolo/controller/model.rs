/** controller/model.rs

Implementação concreta de `Controller` (o trait de `controller/mod.rs`) — só a metade do desenho que
já está fechada: um `Controller` declara, na própria construção, quais `Sensor`s precisa consumir e
quais `Actuator`s precisa comandar, por nome (`StateRegistry::need_sensor()`/`need_actuator()`,
mesmo ciclo declare → register → resolve → inject de qualquer outro `need`) — e nada além disso.
Não há `step()`/`update()`, não há frequência de execução, não há lógica de controle nenhuma: quem
quiser ler/escrever de verdade pega os handles guardados aqui (`sensor()`/`actuator()`) e decide o
que fazer, fora deste tipo, até esse design existir.
*/

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::actuator::Actuator;
use crate::dynamic_model::DynamicModel;
use crate::sensor::Sensor;
use crate::state_registry::{ActuatorHandle, SensorHandle, StateRegistry};

pub struct Controller {
    sensors: HashMap<String, SensorHandle>,
    actuators: HashMap<String, ActuatorHandle>,
}

impl Controller {
    /** `sensors`/`actuators` são os nomes de catálogo que este controller declara precisar —
    resolvidos depois, junto com tudo mais, pela mesma `StateRegistry::resolve()`. Não erra na hora;
    só `resolve()` pode falhar, se algum nome nunca for oferecido.

    `name` é o nome de catálogo do próprio controller — devolve `Rc<Self>`, não `Self`: mesma
    invariante de `Sensor`/`Actuator` (Art. 7.1 §1/12.1 §1), "criado = já oferecido". `new()` já
    registra o controller sob `name` (`offer_controller()`) antes de devolver.
    */
    pub fn new(
        registry: &mut StateRegistry,
        name: &str,
        sensors: &[&str],
        actuators: &[&str],
    ) -> Rc<Self> {
        let sensors = sensors
            .iter()
            .map(|&name| (name.to_string(), registry.need_sensor(name)))
            .collect();
        let actuators = actuators
            .iter()
            .map(|&name| (name.to_string(), registry.need_actuator(name)))
            .collect();

        let controller = Rc::new(Self { sensors, actuators });
        registry.offer_controller(name, controller.clone());
        controller
    }

    /* `None` se `name` não foi declarado em `new()` — não distingue isso de "ainda não resolvido"
    (checar antes de resolve() é erro de uso, não de configuração; ver debug_assert em
    SensorHandle/ActuatorHandle).
    */
    pub fn sensor(&self, name: &str) -> Option<Arc<dyn Sensor>> {
        self.sensors.get(name).map(SensorHandle::sensor)
    }

    pub fn actuator(&self, name: &str) -> Option<Rc<dyn Actuator>> {
        self.actuators.get(name).map(ActuatorHandle::actuator)
    }
}

/** Fase (C) da árvore de avaliação (dynamic_model.rs/component.rs) é definida como "Controllers :
DynamicModel", mesmo sem lógica de controle nenhuma existir ainda — `evaluate()` vazio de propósito:
ocupa a fase estruturalmente (sempre depois de todos os Actuators), pronto pra quando step()/PID/
frequência de execução existirem, sem inventar essa semântica agora. Um único `impl` aqui cobre TODO
Controller, escrito à mão ou via `#[controller(...)]` (`monjolo-macros`, que só embrulha
`Controller::new()` — nunca poderia emitir este `impl` sozinho: trait E tipo são de `monjolo`, fora
do crate de quem usa a macro, e um `impl` por invocação colidiria com os outros).
*/
impl DynamicModel for Controller {
    fn evaluate(&self) {}
}

impl super::Controller for Controller {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Controller as ControllerTrait;

    struct DummySensor;
    impl Sensor for DummySensor {
        fn read(&self) -> f64 {
            2705.0
        }
    }

    struct DummyActuator;
    impl Actuator for DummyActuator {
        fn write(&self, _value: f64) {}
    }

    /** Prova o ponto central: um Controller declara nomes antes de qualquer offer_sensor()/
    offer_actuator() correspondente, e ainda assim resolve certo — mesma ordem-independência de
    qualquer outro need (Art. 6.3).
    */
    #[test]
    fn resolves_named_dependencies_declared_before_they_are_offered() {
        let registry = StateRegistry::shared();
        let controller = Controller::new(
            &mut registry.borrow_mut(),
            "reactor_pressure_control",
            &["reactor_pressure"],
            &["purge"],
        );

        registry.borrow_mut().offer_sensor("reactor_pressure", Arc::new(DummySensor));
        registry.borrow_mut().offer_actuator("purge", Rc::new(DummyActuator));
        registry.borrow_mut().resolve().unwrap();

        assert_eq!(controller.sensor("reactor_pressure").unwrap().read(), 2705.0);
        assert!(controller.actuator("purge").is_some());
        assert!(controller.sensor("missing").is_none());
    }

    /** Prova a invariante nova: `Controller::new()` já registra o controller no catálogo de
    `StateRegistry`, sob `name` — ninguém precisa chamar `offer_controller()` à parte.
    */
    #[test]
    fn new_registers_itself_under_name() {
        let registry = StateRegistry::shared();
        let controller = Controller::new(&mut registry.borrow_mut(), "reactor_pressure_control", &[], &[]);
        let controller: Rc<dyn ControllerTrait> = controller;

        let found = registry
            .borrow()
            .controller("reactor_pressure_control")
            .expect("deveria estar no catálogo");
        assert!(Rc::ptr_eq(&controller, &found));
    }
}
