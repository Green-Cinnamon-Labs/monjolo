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
// Simulation é também o objeto que mantém sensores/atuadores registrados
// (seção 10 do plano) — dono de uma `IoImage`. Um adaptador externo (ex.: o
// servidor OPC-UA em tep-opcuaServer) chama `add_sensor`/`add_actuator` uma
// vez, na montagem, e depois só usa `io()` pra listar/ler/escrever sinais a
// cada tick, sem tocar em StateRegistry/DynamicModel diretamente.
//
// EM TRANSIÇÃO: falta o Integrator de verdade (RK4 ainda está comentado, com
// assinatura antiga) — `run()` por enquanto só faz uma rodada de avaliação e
// commita, sem integrar nada no tempo.

use std::cell::RefCell;
use std::rc::Rc;

use crate::dynamic_model::DynamicModel;
use crate::io_image::{CommandSink, IoImage};
use crate::sensor::model::{Sensor, SensorBehavior};
use crate::state_registry::StateRegistry;

pub struct Simulation {
    model: Box<dyn DynamicModel>,
    registry: Rc<RefCell<StateRegistry>>,
    io: IoImage,
}

impl Simulation {
    pub fn new<M>(build: impl FnOnce(&mut StateRegistry) -> M) -> Result<Self, String>
    where
        M: DynamicModel + 'static,
    {
        let registry = StateRegistry::shared();
        let model: Box<dyn DynamicModel> = Box::new(build(&mut registry.borrow_mut()));
        registry.borrow_mut().resolve()?;
        Ok(Self { model, registry, io: IoImage::new() })
    }

    /** Publica um `Sensor` observando `key`, sob o nome `name`, na `IoImage`
    desta simulação. Só chame depois que `new()` retornar — `resolve()` já
    rodou nesse ponto (seção 3.8 do plano), então `key` precisa já existir.
    */
    pub fn add_sensor(
        &mut self,
        name: &str,
        key: &str,
        behavior: Box<dyn SensorBehavior>,
    ) -> Result<(), String> {
        let sensor = Sensor::new(self.registry.clone(), key, behavior)?;
        self.io.publish_sensor(name, sensor);
        Ok(())
    }

    /// Registra um sink de comando sob `name` na `IoImage` desta simulação.
    pub fn add_actuator(&mut self, name: &str, sink: impl CommandSink + 'static) {
        self.io.register_actuator(name, sink);
    }

    /// Acesso à `IoImage` — um adaptador externo (ex.: servidor OPC-UA) usa
    /// isso pra listar/ler/escrever sinais, tipicamente depois de cada
    /// `run()`.
    pub fn io(&mut self) -> &mut IoImage {
        &mut self.io
    }

    /// TODO: isso ainda não é um loop de integração de verdade — falta o
    /// Integrator (RK4). Por enquanto só roda uma avaliação e commita, pra
    /// provar que a cadeia inteira funciona de ponta a ponta.
    pub fn run(&self) {
        self.model.evaluate();
        self.registry.borrow_mut().commit();
    }
}
