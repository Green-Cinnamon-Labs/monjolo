// simulation-framework/simulation.rs
//
// Interface externa do framework — o que uma planta (ex.: TennesseeEastmanModel)
// usa pra rodar de verdade. Tudo em dynamic_model.rs/state_registry.rs/method/
// actuator/sensor/disturbance é implementação interna; Simulation é a fachada
// pública que junta o StateRegistry com o resto (ver
// docs/issue55_opcua_refactor/plan_refactor.md, seção 9).
//
// Simulation possui o ciclo de vida inteiro: cria o próprio StateRegistry,
// constrói o modelo com ele (via `build`, injetado por quem chama — ex.:
// `TennesseeEastmanModel::new`) e resolve, tudo dentro de `new()`. Quem
// chama não precisa saber que StateRegistry existe: só passa "como construir
// o modelo" e recebe de volta uma Simulation pronta pra rodar.
//
// EM TRANSIÇÃO: falta o Integrator de verdade (RK4 ainda está comentado, com
// assinatura antiga) — `run()` por enquanto só faz uma rodada de avaliação e
// commita, sem integrar nada no tempo.

use std::cell::RefCell;
use std::rc::Rc;

use crate::dynamic_model::DynamicModel;
use crate::state_registry::StateRegistry;

pub struct Simulation {
    model: Box<dyn DynamicModel>,
    registry: Rc<RefCell<StateRegistry>>,
}

impl Simulation {
    pub fn new<M>(build: impl FnOnce(&mut StateRegistry) -> M) -> Result<Self, String>
    where
        M: DynamicModel + 'static,
    {
        let registry = StateRegistry::shared();
        let model: Box<dyn DynamicModel> = Box::new(build(&mut registry.borrow_mut()));
        registry.borrow_mut().resolve()?;
        Ok(Self { model, registry })
    }

    /// TODO: isso ainda não é um loop de integração de verdade — falta o
    /// Integrator (RK4). Por enquanto só roda uma avaliação e commita, pra
    /// provar que a cadeia inteira funciona de ponta a ponta.
    pub fn run(&self) {
        self.model.evaluate();
        self.registry.borrow_mut().commit();
    }
}
