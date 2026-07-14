// monjolo/simulation.rs
//
// Interface externa do framework — a fachada/builder pública que quem monta
// uma planta (ex.: TennesseeEastmanModel) usa pra rodar de verdade. Tudo em
// dynamic_model.rs/state_registry.rs/numerical_method/actuator/sensor/
// disturbance é implementação interna (ver
// docs/issue55_opcua_refactor/plan_refactor.md, seção 9-10).
//
// Simulation é o lifecycle manager do framework: um BUILDER até `run()` ser
// chamado (`set_model()`, `set_adapter()`, `add_sensor()`, `add_actuator()`
// só guardam definições/fábricas, nada é instanciado ainda), e depois disso
// o supervisor que decide quais serviços internos subir e detecta quando
// algum deles morre.
//
// `run()` sobe até dois serviços, cada um numa thread própria, só se tiver
// sido configurado: a "Thread da planta" (se `set_model()` foi chamado) e a
// "Thread do adapter" (se `set_adapter()` foi chamado — hoje só existe
// AdapterConfig::OpcUa). As duas só se comunicam por SnapshotBus (leitura)
// e CommandQueue (escrita) — nunca StateRegistry direto, porque
// `Rc<RefCell<StateRegistry>>`, `IoImage`, `Sensor`, `Proxy` e o
// `Box<dyn DynamicModel>` concreto não são `Send`: só podem nascer e viver
// dentro da thread que os criou. Por isso `set_model`/`add_sensor`/
// `add_actuator` recebem `+ Send` nas closures/comportamentos — são o que
// cruza a fronteira; o que elas produzem nunca precisa ser Send.
//
// Sensores vêm de dois lugares, fundidos dentro da "Thread da planta": os
// que `add_sensor()` declarou aqui de fora, e os que o próprio modelo
// declara via `DynamicModel::sensors()` (ex.: TennesseeEastmanModel — ver
// plan_refactor.md, seção 11.8). `set_model()` captura `model.sensors()` e
// `model.state_keys()` enquanto o tipo ainda é concreto (antes de virar
// `Box<dyn DynamicModel>`, que já apagou esse acesso).
//
// Integrator (RK4, seção 8-9 do plano): `tick_interval` é só o ritmo de
// parede (quanto a thread dorme entre rodadas) — nunca o passo físico de
// integração, que teria unidade errada (segundos de parede != horas de
// processo). `dt_hours` é o passo simulado de verdade, decidido à parte.
//
// Supervisor (lifecycle): cada thread interna manda exatamente um
// `ServiceEvent` pro canal de lifecycle como último passo antes de
// retornar — seja por retorno normal, erro fatal sem pânico, ou pânico de
// verdade (capturado via `std::panic::catch_unwind`, nunca deixado vazar
// pra fora da thread). `run()` bloqueia em `events_rx.recv()`: o primeiro
// evento que chegar (de qualquer serviço configurado) já é motivo pra
// `run()` parar de esperar e devolver. Não existe cancelamento cooperativo
// ainda — nem a plant thread nem o adapter checam um sinal de parada — por
// isso `run()` não força a thread sobrevivente a morrer, só para de
// esperar por ela (ver comentário dentro de `run()`).

use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(feature = "opcua")]
use crate::adapter::command_queue::CommandQueue;
use crate::adapter::snapshot_bus::SnapshotBus;
use crate::adapter::AdapterConfig;
use crate::dynamic_model::DynamicModel;
use crate::io_image::{CommandSink, IoImage};
use crate::numerical_method::NumericalMethod;
use crate::sensor::model::{Ideal, Sensor, SensorBehavior};
use crate::state_registry::{Proxy, StateRegistry};

type ModelFactory = dyn FnOnce(&mut StateRegistry) -> (Box<dyn DynamicModel>, Vec<(String, String)>, Vec<String>)
    + Send;
type SensorSpec = (String, String, Box<dyn SensorBehavior + Send>);
type ActuatorSpec = (String, Box<dyn CommandSink + Send>);

/// Identifica qual serviço interno gerou um `ServiceEvent` — só usado pra
/// decidir, depois que `run()` já sabe que algo encerrou, qual `JoinHandle`
/// dos dois faz sentido juntar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceKind {
    Plant,
    Adapter,
}

/** Evento de fim de vida de uma thread interna — a "Thread da planta" e a
"Thread do adapter" mandam exatamente um destes, como último passo antes de
retornar. `run()` bloqueia em `events_rx.recv()` esperando o primeiro que
chegar — é assim que ele percebe uma thread morta sem precisar de polling.
*/
enum ServiceEvent {
    /// Terminou sem erro — hoje nenhum dos dois serviços tem um caminho de
    /// saída normal de verdade (a plant thread roda um `loop {}` sem
    /// break, o adapter só sai por erro), mas o tipo comporta pra quando
    /// isso deixar de ser verdade.
    Stopped(ServiceKind),
    /// Encerrou por um erro que o próprio serviço detectou e decidiu
    /// devolver como `Err` — não um pânico de linguagem.
    Failed(ServiceKind, String),
    /// Entrou em pânico — capturado por `catch_unwind`, nunca deixado
    /// vazar pra fora da thread.
    Panicked(ServiceKind, String),
}

impl ServiceEvent {
    fn service(&self) -> ServiceKind {
        match self {
            ServiceEvent::Stopped(kind)
            | ServiceEvent::Failed(kind, _)
            | ServiceEvent::Panicked(kind, _) => *kind,
        }
    }
}

/// Extrai uma mensagem legível do payload de um pânico capturado por
/// `catch_unwind` — `panic!("...")`/`panic!("{}", x)` produzem `&str` ou
/// `String`; qualquer outro tipo (raro — ex.: `panic_any` com um tipo
/// próprio) cai no fallback.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "pânico sem mensagem legível (payload não é &str nem String)".to_string()
    }
}

pub struct Simulation {
    model_factory: Option<Box<ModelFactory>>,
    sensor_specs: Vec<SensorSpec>,
    actuator_specs: Vec<ActuatorSpec>,
    tick_interval: Duration,
    dt_hours: f64,
    numerical_method: NumericalMethod,
    adapter_config: Option<AdapterConfig>,
}

impl Default for Simulation {
    fn default() -> Self {
        Self {
            model_factory: None,
            sensor_specs: Vec::new(),
            actuator_specs: Vec::new(),
            tick_interval: Duration::from_millis(500),
            dt_hours: 1.0 / 3600.0,
            numerical_method: NumericalMethod::default(),
            adapter_config: None,
        }
    }
}

impl Simulation {
    pub fn new() -> Self {
        Self::default()
    }

    /** Passo físico simulado por tick, em horas — a unidade que o resto da
    física do TEP usa (ex.: `VALVE_TIME_CONSTANTS`). Não confundir com
    `tick_interval` (ritmo de parede, `std::thread::sleep`): os dois são
    independentes de propósito — quão rápido a thread roda não deveria
    mudar quanto tempo de processo cada passo avança. Default: 1 segundo
    simulado por tick (1.0 / 3600.0 horas).
    */
    pub fn set_dt_hours(&mut self, dt_hours: f64) {
        self.dt_hours = dt_hours;
    }

    /// Ritmo de parede entre rodadas (`std::thread::sleep`) — não tem
    /// relação com `dt_hours`, ver comentário no topo do arquivo. Default:
    /// 500ms.
    pub fn set_tick_interval(&mut self, interval: Duration) {
        self.tick_interval = interval;
    }

    /** Escolhe o método numérico de integração — só aceita o que
    `NumericalMethod` (enum fechado, `numerical_method/mod.rs`) já
    implementa dentro do framework, nunca uma implementação arbitrária de
    fora. Default: `NumericalMethod::RK4`. `run()` consome isso via
    `NumericalMethod::integrator()` dentro da "Thread da planta".
    */
    pub fn set_numerical_method(&mut self, method: NumericalMethod) {
        self.numerical_method = method;
    }

    /** Registra a infraestrutura externa (hoje só `AdapterConfig::OpcUa`)
    que `run()` deve subir numa thread própria, falando com a plant thread
    só por `SnapshotBus`/`CommandQueue` — nunca por `StateRegistry` direto.
    Só aceita o que `AdapterConfig` já implementa dentro do framework, mesmo
    raciocínio de `set_numerical_method`.
    */
    pub fn set_adapter(&mut self, config: AdapterConfig) {
        self.adapter_config = Some(config);
    }

    /** Define a fábrica do modelo — chamada só depois, dentro da "Thread da
    planta", com o `StateRegistry` já criado nesse contexto. Ex.:
    `simulation.set_model(TennesseeEastmanModel::new)`.

    Também captura `model.sensors()` e `model.state_keys()` — o que o
    próprio modelo declara (`DynamicModel`, defaults vazios) — enquanto o
    tipo ainda é `M` concreto, antes de virar `Box<dyn DynamicModel>` (que já
    não permite mais chamar métodos além do trait).
    */
    pub fn set_model<M>(&mut self, factory: impl FnOnce(&mut StateRegistry) -> M + Send + 'static)
    where
        M: DynamicModel + 'static,
    {
        self.model_factory = Some(Box::new(move |registry: &mut StateRegistry| {
            let model = factory(registry);
            let sensors = model.sensors();
            let state_keys = model.state_keys();
            (
                Box::new(model) as Box<dyn DynamicModel>,
                sensors,
                state_keys,
            )
        }));
    }

    /** Declara um sensor sob `name`, observando `key` — só guarda a
    definição; o `Sensor` de verdade (`ReadProxy` resolvido, seção 3.6.2 do
    plano) só nasce dentro da "Thread da planta", em `run()`, depois que o
    modelo já se inscreveu no `StateRegistry`. Use isso pra sensores que não
    fazem parte da declaração do próprio modelo (`sensors()`) — os dois se
    fundem em `run()`.
    */
    pub fn add_sensor(
        &mut self,
        name: &str,
        key: &str,
        behavior: impl SensorBehavior + Send + 'static,
    ) {
        self.sensor_specs
            .push((name.to_string(), key.to_string(), Box::new(behavior)));
    }

    /** Declara um atuador sob `name` — mesma lógica de `add_sensor`: só
    guarda a definição, o `CommandSink` só é registrado na `IoImage`
    dentro da "Thread da planta".
    */
    pub fn add_actuator(&mut self, name: &str, sink: impl CommandSink + Send + 'static) {
        self.actuator_specs.push((name.to_string(), Box::new(sink)));
    }

    /** Chamada terminal — consome a `Simulation` (builder) e orquestra os
    serviços internos configurados. Sobe a "Thread da planta" só se
    `set_model()` foi chamado; sobe a "Thread do adapter" só se
    `set_adapter()` foi chamado; se nenhum dos dois foi configurado, devolve
    `Err` sem subir thread nenhuma.

    Bloqueia até o primeiro serviço configurado encerrar — normalmente,
    erro fatal ou pânico (capturado, nunca propagado como pânico de
    verdade). `Ok(())` só no caso raro de um serviço encerrar limpo;
    qualquer erro ou pânico vira `Err` descrevendo qual serviço e por quê.

    NÃO EXISTE cancelamento cooperativo ainda: se dois serviços estão
    configurados e um deles morre, o outro não é avisado — `run()` só para
    de esperar por ele (nunca chama `.join()` nele, bloquearia pra sempre
    enquanto ele seguir saudável) e devolve o resultado. Cabe a quem chamou
    `run()` decidir encerrar o processo, o que mata a thread órfã junto
    (caso comum de um binário).
    */
    pub fn run(mut self) -> Result<(), String> {
        let model_factory = self.model_factory.take();
        let adapter_config = self.adapter_config.take();

        if model_factory.is_none() && adapter_config.is_none() {
            return Err(
                "run: nada configurado — chame set_model() e/ou set_adapter() antes".to_string(),
            );
        }

        eprintln!(
            "[main] Simulation::run — modelo: {}, adapter: {:?}, método numérico: {:?}",
            if model_factory.is_some() {
                "configurado"
            } else {
                "nenhum"
            },
            adapter_config,
            self.numerical_method,
        );

        let external_sensor_specs = std::mem::take(&mut self.sensor_specs);
        let actuator_specs = std::mem::take(&mut self.actuator_specs);
        let actuator_names: Vec<String> = actuator_specs
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let tick_interval = self.tick_interval;
        let dt_hours = self.dt_hours;
        let numerical_method = self.numerical_method;

        let (events_tx, events_rx) = std::sync::mpsc::channel::<ServiceEvent>();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<(String, f64)>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Vec<String>>();
        let snapshot = SnapshotBus::new();

        let plant_handle = model_factory.map(|factory| {
            Self::spawn_plant_thread(
                factory,
                external_sensor_specs,
                actuator_specs,
                tick_interval,
                dt_hours,
                numerical_method,
                snapshot.clone(),
                cmd_rx,
                ready_tx,
                events_tx.clone(),
            )
        });

        let adapter_handle = Self::spawn_adapter_if_configured(
            adapter_config,
            &plant_handle,
            ready_rx,
            actuator_names,
            cmd_tx,
            snapshot,
            events_tx,
        );

        let first_event = events_rx.recv().map_err(|_| {
            "run: nenhum serviço interno reportou nada — canal de lifecycle fechado inesperadamente".to_string()
        })?;

        // A thread que já mandou seu evento está a um passo de retornar (foi
        // o último passo antes disso) — juntar ela é rápido e seguro. A
        // outra, se existir, segue rodando sem supervisão (ver comentário
        // no doc de `run()`).
        match first_event.service() {
            ServiceKind::Plant => {
                if let Some(handle) = plant_handle {
                    let _ = handle.join();
                }
            }
            ServiceKind::Adapter => {
                if let Some(handle) = adapter_handle {
                    let _ = handle.join();
                }
            }
        }

        match first_event {
            ServiceEvent::Stopped(_) => Ok(()),
            ServiceEvent::Failed(service, reason) => {
                Err(format!("{service:?}: encerrou com erro fatal: {reason}"))
            }
            ServiceEvent::Panicked(service, reason) => {
                Err(format!("{service:?}: entrou em pânico: {reason}"))
            }
        }
    }

    /** Sobe a "Thread da planta": cria `StateRegistry`, o modelo, `IoImage`
    e os sensores/atuadores de verdade (nada disso existe antes desse
    ponto), manda os nomes de sensor pro adapter via `ready_tx` (se algum
    estiver esperando — inofensivo se ninguém estiver do outro lado) e entra
    no loop de tick (integra via RK4 o que o modelo declarou em
    `state_keys()`, ou só avalia se não há nada pra integrar).

    O corpo inteiro roda dentro de `catch_unwind` — um pânico aqui (seja na
    inscrição inicial, seja em qualquer tick depois) nunca escapa da thread:
    vira um `ServiceEvent::Panicked` mandado pro canal de lifecycle.
    */
    fn spawn_plant_thread(
        model_factory: Box<ModelFactory>,
        external_sensor_specs: Vec<SensorSpec>,
        actuator_specs: Vec<ActuatorSpec>,
        tick_interval: Duration,
        dt_hours: f64,
        numerical_method: NumericalMethod,
        snapshot: SnapshotBus,
        cmd_rx: Receiver<(String, f64)>,
        ready_tx: Sender<Vec<String>>,
        events: Sender<ServiceEvent>,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("plant".to_string())
            .spawn(move || {
            let outcome = panic::catch_unwind(AssertUnwindSafe(move || {
                let registry = StateRegistry::shared();
                let (model, model_sensor_specs, model_state_keys) =
                    model_factory(&mut registry.borrow_mut());

                // Cada chave de estado integrável precisa de uma
                // contraparte ".derivative" (seção 8.3 do plano) — pede as
                // duas como `need` aqui, antes do resolve() geral, pra sair
                // com Proxy pareado (estado, derivada) na mesma ordem de
                // model_state_keys.
                let mut integration_needs: Vec<String> =
                    Vec::with_capacity(model_state_keys.len() * 2);
                for key in &model_state_keys {
                    integration_needs.push(key.clone());
                    integration_needs.push(format!("{key}.derivative"));
                }
                let integration_need_refs: Vec<&str> =
                    integration_needs.iter().map(String::as_str).collect();
                let (_, integration_proxies) =
                    registry.borrow_mut().subscribe(&[], &integration_need_refs);

                registry
                    .borrow_mut()
                    .resolve()
                    .expect("plant thread: falha ao resolver o StateRegistry — algum `need` não tem provedor");

                let mut state_proxies: Vec<Proxy> = Vec::with_capacity(model_state_keys.len());
                let mut derivative_proxies: Vec<Proxy> = Vec::with_capacity(model_state_keys.len());
                for pair in integration_proxies.chunks(2) {
                    state_proxies.push(pair[0].clone());
                    derivative_proxies.push(pair[1].clone());
                }
                let integrator = numerical_method.integrator();

                let mut io = IoImage::new();
                let mut sensor_names = Vec::new();

                for (name, key, behavior) in external_sensor_specs {
                    let sensor =
                        Sensor::new(registry.clone(), &key, behavior).unwrap_or_else(|e| {
                            panic!("plant thread: sensor '{name}' (chave '{key}'): {e}")
                        });
                    io.register_sensor(&name, sensor);
                    sensor_names.push(name);
                }
                for (name, key) in model_sensor_specs {
                    let sensor = Sensor::new(registry.clone(), &key, Box::new(Ideal))
                        .unwrap_or_else(|e| {
                            panic!("plant thread: sensor do modelo '{name}' (chave '{key}'): {e}")
                        });
                    io.register_sensor(&name, sensor);
                    sensor_names.push(name);
                }
                for (name, sink) in actuator_specs {
                    io.register_actuator(&name, sink);
                }

                let _ = ready_tx.send(sensor_names.clone());

                eprintln!(
                    "[plant] iniciando — {} sensor(es), {} atuador(es), {} chave(s) de estado integrável, tick a cada {tick_interval:?} (dt = {dt_hours}h)",
                    sensor_names.len(),
                    io.actuator_names().count(),
                    state_proxies.len(),
                );

                loop {
                    while let Ok((name, value)) = cmd_rx.try_recv() {
                        let _ = io.write(&name, value);
                    }

                    if state_proxies.is_empty() {
                        // Nenhum componente do modelo declarou state_keys()
                        // — não há nada pra integrar, só avalia a árvore
                        // uma vez (mesmo comportamento de antes do
                        // Integrator existir).
                        model.evaluate();
                    } else {
                        let current: Vec<f64> = state_proxies.iter().map(Proxy::get).collect();

                        // A closure é o "dynamics" da seção 9.6: escreve o
                        // estado perturbado (um k-ésimo sub-passo do RK4)
                        // nos Proxys de estado, dispara evaluate() da
                        // árvore inteira (que lê esse estado e recalcula
                        // tudo, inclusive as derivadas) e devolve as
                        // derivadas resultantes.
                        let next =
                            integrator.step(&current, dt_hours, &mut |perturbed: &[f64]| {
                                for (proxy, &value) in state_proxies.iter().zip(perturbed) {
                                    proxy.set(value);
                                }
                                model.evaluate();
                                derivative_proxies.iter().map(Proxy::get).collect()
                            });

                        // O último evaluate() acima rodou sobre s4 (um
                        // sub-passo hipotético do RK4, não o estado final
                        // combinado) — escreve o estado de verdade e
                        // reavalia mais uma vez pra EvaluationState
                        // refletir o que vai ser commitado, não o resíduo
                        // do último k4.
                        for (proxy, &value) in state_proxies.iter().zip(&next) {
                            proxy.set(value);
                        }
                        model.evaluate();
                    }

                    registry.borrow_mut().commit();

                    let readings: Vec<(&str, f64)> = sensor_names
                        .iter()
                        .filter_map(|name| io.read(name).map(|value| (name.as_str(), value)))
                        .collect();
                    snapshot.publish_all(readings);

                    std::thread::sleep(tick_interval);
                }
            }));

            let event = match outcome {
                Ok(()) => ServiceEvent::Stopped(ServiceKind::Plant),
                Err(payload) => ServiceEvent::Panicked(ServiceKind::Plant, panic_message(payload)),
            };
            let _ = events.send(event);
        })
            .expect("run: falha ao criar a thread da planta")
    }

    /** Sobe a "Thread do adapter" se `adapter_config` estiver presente —
     `None` caso contrário, sem tocar em `ready_rx`/`cmd_tx`/etc. Espera o
     handshake de nomes de sensor via `ready_rx` só se existe uma plant
     thread de verdade (`plant_handle.is_some()`); sem planta, sobe com
     zero sensores. Se a planta morreu antes do handshake (`ready_rx`
     fechado), nem chega a subir o adapter — o pânico dela já está a
     caminho do canal de lifecycle.
    */
    #[cfg(feature = "opcua")]
    fn spawn_adapter_if_configured(
        adapter_config: Option<AdapterConfig>,
        plant_handle: &Option<JoinHandle<()>>,
        ready_rx: Receiver<Vec<String>>,
        actuator_names: Vec<String>,
        cmd_tx: Sender<(String, f64)>,
        snapshot: SnapshotBus,
        events: Sender<ServiceEvent>,
    ) -> Option<JoinHandle<()>> {
        let config = adapter_config?;
        let sensor_names = if plant_handle.is_some() {
            ready_rx.recv().ok()?
        } else {
            Vec::new()
        };
        let commands = CommandQueue::new(cmd_tx);
        Some(Self::spawn_adapter_thread(
            config,
            sensor_names,
            actuator_names,
            snapshot,
            commands,
            events,
        ))
    }

    #[cfg(not(feature = "opcua"))]
    fn spawn_adapter_if_configured(
        _adapter_config: Option<AdapterConfig>,
        _plant_handle: &Option<JoinHandle<()>>,
        _ready_rx: Receiver<Vec<String>>,
        _actuator_names: Vec<String>,
        _cmd_tx: Sender<(String, f64)>,
        _snapshot: SnapshotBus,
        _events: Sender<ServiceEvent>,
    ) -> Option<JoinHandle<()>> {
        None
    }

    /** Sobe o runtime Tokio + servidor do adapter numa thread própria. O
    corpo inteiro (criação do runtime + `serve()`) roda dentro de
    `catch_unwind`, igual à plant thread — um `Err` de `serve()` vira
    `ServiceEvent::Failed` (erro fatal, sem pânico); um pânico de verdade
    vira `ServiceEvent::Panicked`. Nunca eprintln!+segue como antes — todo
    desfecho passa pelo canal de lifecycle.
    */
    #[cfg(feature = "opcua")]
    fn spawn_adapter_thread(
        config: AdapterConfig,
        sensor_names: Vec<String>,
        actuator_names: Vec<String>,
        snapshot: SnapshotBus,
        commands: CommandQueue,
        events: Sender<ServiceEvent>,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("adapter".to_string())
            .spawn(move || {
                let outcome =
                    panic::catch_unwind(AssertUnwindSafe(move || -> Result<(), String> {
                        let endpoint = match config {
                            AdapterConfig::OpcUa { endpoint } => endpoint,
                        };

                        eprintln!(
                    "[adapter] iniciando — OPC-UA em {endpoint}, {} sensor(es), {} atuador(es)",
                    sensor_names.len(),
                    actuator_names.len(),
                );

                        let runtime = tokio::runtime::Runtime::new()
                            .map_err(|e| format!("falha ao criar runtime tokio pro OPC-UA: {e}"))?;
                        runtime.block_on(crate::adapter::opcua::serve(
                            sensor_names,
                            actuator_names,
                            snapshot,
                            commands,
                            &endpoint,
                        ))
                    }));

                let event = match outcome {
                    Ok(Ok(())) => ServiceEvent::Stopped(ServiceKind::Adapter),
                    Ok(Err(reason)) => ServiceEvent::Failed(ServiceKind::Adapter, reason),
                    Err(payload) => {
                        ServiceEvent::Panicked(ServiceKind::Adapter, panic_message(payload))
                    }
                };
                let _ = events.send(event);
            })
            .expect("run: falha ao criar a thread do adapter")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Modelo mínimo só pra provar que `run()` tica de verdade — não tem
    /// estado no StateRegistry nenhum, só conta quantas vezes `evaluate()`
    /// foi chamado.
    struct CountingModel {
        ticks: Arc<AtomicUsize>,
    }

    impl DynamicModel for CountingModel {
        fn evaluate(&self) {
            self.ticks.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn run_requires_model_or_adapter() {
        let simulation = Simulation::new();
        assert!(simulation.run().is_err());
    }

    #[test]
    fn run_ticks_on_its_own_thread() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let ticks_for_build = ticks.clone();

        let mut simulation = Simulation::new();
        // Arc<AtomicUsize> é Send — atravessa a fronteira dentro de
        // set_model mesmo o CountingModel resultante não sendo Send.
        simulation.set_model(move |_registry| CountingModel {
            ticks: ticks_for_build.clone(),
        });

        let _handle = std::thread::spawn(move || {
            let _ = simulation.run();
        });

        std::thread::sleep(Duration::from_millis(100));
        let count = ticks.load(Ordering::SeqCst);
        assert!(
            count >= 1,
            "esperava pelo menos um tick em 100ms, contou {count}"
        );
    }

    /// dv/dt = -v, nasce em 100.0 — declara `state_keys()` (o que Valve/
    /// Agitator já fazem hoje, seção 8.3 do plano), mas ninguém até agora
    /// integrava essa derivada de verdade (era a lacuna: "quem soma o
    /// estado no tempo?"). Guarda o último valor observado num
    /// Arc<Mutex<f64>> pra provar, de fora da thread da planta, que run()
    /// está mesmo chamando o Integrator a cada tick.
    struct DecayModel {
        value: Proxy,
        derivative: Proxy,
        observed: Arc<std::sync::Mutex<f64>>,
    }

    impl DecayModel {
        fn new(registry: &mut StateRegistry, observed: Arc<std::sync::Mutex<f64>>) -> Self {
            let (offered, _) = registry.subscribe(&["decay.value", "decay.value.derivative"], &[]);
            offered[0].set(100.0);
            Self {
                value: offered[0].clone(),
                derivative: offered[1].clone(),
                observed,
            }
        }
    }

    impl DynamicModel for DecayModel {
        fn evaluate(&self) {
            let value = self.value.get();
            self.derivative.set(-value);
            *self.observed.lock().unwrap() = value;
        }

        fn state_keys(&self) -> Vec<String> {
            vec!["decay.value".to_string()]
        }
    }

    #[test]
    fn run_integrates_declared_state_keys_via_rk4() {
        let observed = Arc::new(std::sync::Mutex::new(100.0));
        let observed_for_build = observed.clone();

        let mut simulation = Simulation::new();
        simulation.set_tick_interval(Duration::from_millis(5));
        simulation.set_dt_hours(0.1);
        simulation.set_model(move |registry| DecayModel::new(registry, observed_for_build.clone()));

        let _handle = std::thread::spawn(move || {
            let _ = simulation.run();
        });

        std::thread::sleep(Duration::from_millis(200));
        let value = *observed.lock().unwrap();
        assert!(
            value < 90.0,
            "esperava decaimento perceptível de 100.0, ficou em {value}"
        );
        assert!(
            value > 0.0,
            "dv/dt = -v nunca cruza zero, mas obteve {value}"
        );
    }

    /// Modelo que entra em pânico depois de alguns ticks saudáveis —
    /// simula uma falha real dentro de evaluate(). Prova o supervisor
    /// inteiro: catch_unwind captura o pânico dentro da plant thread,
    /// vira ServiceEvent::Panicked, e run() RETORNA (em vez de travar pra
    /// sempre, que era o comportamento de qualquer pânico não capturado
    /// numa thread sem ninguém dando join nela).
    struct PanickyModel {
        ticks: Arc<AtomicUsize>,
    }

    impl DynamicModel for PanickyModel {
        fn evaluate(&self) {
            let n = self.ticks.fetch_add(1, Ordering::SeqCst);
            if n >= 2 {
                panic!("PanickyModel: pane proposital no tick {n}");
            }
        }
    }

    #[test]
    fn run_returns_err_instead_of_hanging_when_plant_panics() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let mut simulation = Simulation::new();
        simulation.set_tick_interval(Duration::from_millis(1));
        simulation.set_model(move |_registry| PanickyModel {
            ticks: ticks.clone(),
        });

        // Chamado direto (sem thread própria de teste) — se o supervisor
        // não funcionasse, isso travaria o teste pra sempre em vez de
        // devolver um Err.
        let result = simulation.run();

        let message = result.expect_err("esperava Err depois do pânico da PanickyModel");
        assert!(
            message.contains("pane proposital"),
            "mensagem inesperada: {message}"
        );
    }
}
