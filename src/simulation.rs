// simulation-framework/simulation.rs
//
// Interface externa do framework — a fachada/builder pública que quem monta
// uma planta (ex.: TennesseeEastmanModel) usa pra rodar de verdade. Tudo em
// dynamic_model.rs/state_registry.rs/method/actuator/sensor/disturbance é
// implementação interna (ver docs/issue55_opcua_refactor/plan_refactor.md,
// seção 9-10).
//
// Simulation é um BUILDER até `run_model()` ser chamado: `set_model()`,
// `add_sensor()`, `add_actuator()`, `start_opcua_server()` só guardam
// definições/fábricas (closures), nada é instanciado ainda — nenhum
// StateRegistry existe até esse ponto. `run_model()` é a chamada terminal:
// cria o StateRegistry, a IoImage, os sensores/atuadores de verdade, e sobe
// a "Thread da planta" (drawio/dynamicModel.drawio, aba "arquitetura").
//
// Por quê builder e não construção direta: Simulation, depois de montada,
// guarda Rc<RefCell<StateRegistry>> — não é Send, não pode atravessar pra
// dentro de uma thread já construída. A única forma de a planta rodar numa
// thread própria é ela nascer inteira lá dentro. Por isso `set_model`/
// `add_sensor`/`add_actuator` recebem `+ Send` nas closures/comportamentos
// — são o que cruza a fronteira; o que elas produzem (o modelo, os
// Sensor/Proxy) nunca precisa ser Send, porque nunca sai da thread que os
// criou.
//
// Sensores vêm de dois lugares, fundidos dentro da "Thread da planta": os
// que `add_sensor()` declarou aqui de fora, e os que o próprio modelo
// declara via `DynamicModel::sensors()` (ex.: TennesseeEastmanModel — ver
// plan_refactor.md, seção 11.8). `set_model()` captura `model.sensors()`
// enquanto o tipo ainda é concreto (antes de virar `Box<dyn DynamicModel>`,
// que já apagou esse acesso).
//
// A comunicação com o mundo de fora (ex.: OPC-UA) depois que a planta já
// está rodando não é mais `simulation.io()` direto — é por SnapshotBus
// (leitura) e CommandQueue (escrita), as duas únicas coisas thread-safe que
// atravessam pra fora da "Thread da planta".
//
// EM TRANSIÇÃO: falta o Integrator de verdade (RK4) — o tick por enquanto
// só faz uma rodada de avaliação e commita, sem integrar no tempo.

use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(feature = "opcua")]
use crate::command_queue::CommandQueue;
use crate::dynamic_model::DynamicModel;
use crate::io_image::{CommandSink, IoImage};
use crate::sensor::model::{Ideal, Sensor, SensorBehavior};
use crate::snapshot_bus::SnapshotBus;
use crate::state_registry::StateRegistry;

type ModelFactory = dyn FnOnce(&mut StateRegistry) -> (Box<dyn DynamicModel>, Vec<(String, String)>) + Send;
type SensorSpec = (String, String, Box<dyn SensorBehavior + Send>);
type ActuatorSpec = (String, Box<dyn CommandSink + Send>);

#[derive(Default)]
pub struct Simulation {
    model_factory: Option<Box<ModelFactory>>,
    sensor_specs: Vec<SensorSpec>,
    actuator_specs: Vec<ActuatorSpec>,
    tick_interval: Duration,
    #[cfg(feature = "opcua")]
    opcua_endpoint: Option<String>,
}

impl Simulation {
    pub fn new() -> Self {
        Self { tick_interval: Duration::from_millis(500), ..Default::default() }
    }

    /** Define a fábrica do modelo — chamada só depois, dentro da "Thread da
    planta", com o `StateRegistry` já criado nesse contexto. Ex.:
    `simulation.set_model(TennesseeEastmanModel::new)`.

    Também captura `model.sensors()` — os sinais que o próprio modelo
    declara (`DynamicModel::sensors()`, default vazio) — enquanto o tipo
    ainda é `M` concreto, antes de virar `Box<dyn DynamicModel>` (que já não
    permite mais chamar métodos além do trait).
    */
    pub fn set_model<M>(&mut self, factory: impl FnOnce(&mut StateRegistry) -> M + Send + 'static)
    where
        M: DynamicModel + 'static,
    {
        self.model_factory = Some(Box::new(move |registry: &mut StateRegistry| {
            let model = factory(registry);
            let sensors = model.sensors();
            (Box::new(model) as Box<dyn DynamicModel>, sensors)
        }));
    }

    /** Declara um sensor sob `name`, observando `key` — só guarda a
    definição; o `Sensor` de verdade (`ReadProxy` resolvido, seção 3.6.2 do
    plano) só nasce dentro da "Thread da planta", em `run_model()`, depois
    que o modelo já se inscreveu no `StateRegistry`. Use isso pra sensores
    que não fazem parte da declaração do próprio modelo (`sensors()`) — os
    dois se fundem em `run_model()`.
    */
    pub fn add_sensor(&mut self, name: &str, key: &str, behavior: impl SensorBehavior + Send + 'static) {
        self.sensor_specs.push((name.to_string(), key.to_string(), Box::new(behavior)));
    }

    /// Declara um atuador sob `name` — mesma lógica de `add_sensor`: só
    /// guarda a definição, o `CommandSink` só é registrado na `IoImage`
    /// dentro da "Thread da planta".
    pub fn add_actuator(&mut self, name: &str, sink: impl CommandSink + Send + 'static) {
        self.actuator_specs.push((name.to_string(), Box::new(sink)));
    }

    /** Configura (não sobe ainda) um servidor OPC-UA nesse endpoint —
    `opc.tcp://<host>:<porta><path>`. Só passa a existir de verdade quando
    `run_model()` roda: nasce numa "Thread do OPC-UA" separada, falando com
    a planta só por `SnapshotBus`/`CommandQueue`, nunca por `StateRegistry`
    direto.
    */
    #[cfg(feature = "opcua")]
    pub fn start_opcua_server(&mut self, endpoint: &str) {
        self.opcua_endpoint = Some(endpoint.to_string());
    }

    /** Chamada terminal — consome a `Simulation` (builder) e roda de
    verdade. Cria o `StateRegistry`, o modelo, a `IoImage` e os
    sensores/atuadores dentro da "Thread da planta" (nunca antes disso);
    se um servidor OPC-UA foi configurado, sobe também a "Thread do OPC-UA",
    ligada só por `SnapshotBus`/`CommandQueue`.

    Bloqueia até uma das threads encerrar (hoje só por pânico — não existe
    shutdown gracioso ainda). `Err` se nenhum modelo foi definido
    (`set_model()` nunca chamado) ou se alguma thread entrar em pânico.
    */
    pub fn run_model(mut self) -> Result<(), String> {
        let model_factory = self
            .model_factory
            .take()
            .ok_or_else(|| "run_model: nenhum modelo definido — chame set_model() antes".to_string())?;
        let external_sensor_specs = std::mem::take(&mut self.sensor_specs);
        let actuator_specs = std::mem::take(&mut self.actuator_specs);
        let tick_interval = self.tick_interval;

        #[cfg(feature = "opcua")]
        let actuator_names: Vec<String> = actuator_specs.iter().map(|(name, _)| name.clone()).collect();

        #[allow(unused_variables)]
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<(String, f64)>();
        let snapshot = SnapshotBus::new();

        // Handshake de uma via: a "Thread da planta" só sabe o conjunto
        // final de nomes de sensor (externos + declarados pelo modelo)
        // depois de rodar model_factory — e a "Thread do OPC-UA" precisa
        // dessa lista pra montar os nodes antes de subir. Sem isso teria
        // que descobrir os nomes depois, dinamicamente — mais complexo por
        // enquanto sem necessidade real.
        #[cfg(feature = "opcua")]
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Vec<String>>();

        let snapshot_for_plant = snapshot.clone();
        let plant_handle: JoinHandle<()> = std::thread::spawn(move || {
            let registry = StateRegistry::shared();
            let (model, model_sensor_specs) = model_factory(&mut registry.borrow_mut());
            registry
                .borrow_mut()
                .resolve()
                .expect("run_model: falha ao resolver o StateRegistry — algum `need` não tem provedor");

            let mut io = IoImage::new();
            let mut sensor_names = Vec::new();

            for (name, key, behavior) in external_sensor_specs {
                let sensor = Sensor::new(registry.clone(), &key, behavior)
                    .unwrap_or_else(|e| panic!("run_model: sensor '{name}' (chave '{key}'): {e}"));
                io.register_sensor(&name, sensor);
                sensor_names.push(name);
            }
            for (name, key) in model_sensor_specs {
                let sensor = Sensor::new(registry.clone(), &key, Box::new(Ideal))
                    .unwrap_or_else(|e| panic!("run_model: sensor do modelo '{name}' (chave '{key}'): {e}"));
                io.register_sensor(&name, sensor);
                sensor_names.push(name);
            }
            for (name, sink) in actuator_specs {
                io.register_actuator(&name, sink);
            }

            #[cfg(feature = "opcua")]
            let _ = ready_tx.send(sensor_names.clone());

            loop {
                while let Ok((name, value)) = cmd_rx.try_recv() {
                    let _ = io.write(&name, value);
                }

                model.evaluate();
                registry.borrow_mut().commit();

                for name in &sensor_names {
                    if let Some(value) = io.read(name) {
                        snapshot_for_plant.publish(name, value);
                    }
                }

                std::thread::sleep(tick_interval);
            }
        });

        #[cfg(feature = "opcua")]
        if let Some(endpoint) = self.opcua_endpoint.take() {
            let sensor_names = ready_rx
                .recv()
                .map_err(|_| "run_model: a planta encerrou antes de publicar os sensores".to_string())?;
            let commands = CommandQueue::new(cmd_tx);
            let opcua_handle = std::thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new()
                    .expect("run_model: falha ao criar runtime tokio pro OPC-UA");
                runtime.block_on(async move {
                    if let Err(e) =
                        crate::opcua_adapter::serve(sensor_names, actuator_names, snapshot, commands, &endpoint)
                            .await
                    {
                        eprintln!("servidor OPC-UA encerrou com erro: {e}");
                    }
                });
            });
            return opcua_handle.join().map_err(|_| "thread do OPC-UA entrou em pânico".to_string());
        }

        plant_handle.join().map_err(|_| "thread da planta entrou em pânico".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Modelo mínimo só pra provar que `run_model()` tica de verdade — não
    /// tem estado no StateRegistry nenhum, só conta quantas vezes
    /// `evaluate()` foi chamado.
    struct CountingModel {
        ticks: Arc<AtomicUsize>,
    }

    impl DynamicModel for CountingModel {
        fn evaluate(&self) {
            self.ticks.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn run_model_requires_set_model() {
        let simulation = Simulation::new();
        assert!(simulation.run_model().is_err());
    }

    #[test]
    fn run_model_ticks_on_its_own_thread() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let ticks_for_build = ticks.clone();

        let mut simulation = Simulation::new();
        // Arc<AtomicUsize> é Send — atravessa a fronteira dentro de
        // set_model mesmo o CountingModel resultante não sendo Send.
        simulation.set_model(move |_registry| CountingModel { ticks: ticks_for_build.clone() });

        let _handle = std::thread::spawn(move || {
            let _ = simulation.run_model();
        });

        std::thread::sleep(Duration::from_millis(100));
        let count = ticks.load(Ordering::SeqCst);
        assert!(count >= 1, "esperava pelo menos um tick em 100ms, contou {count}");
    }
}
