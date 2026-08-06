// monjolo/simulation.rs
//
// Interface externa do framework — a fachada/builder pública que quem monta
// uma planta (ex.: TennesseeEastmanModel) usa pra rodar de verdade. Tudo em
// dynamic_model.rs/state_registry.rs/numerical_method/actuator/sensor/
// disturbance é implementação interna.
//
// Simulation é o lifecycle manager do framework: um BUILDER até `run()` ser
// chamado (`set_model()` só guarda a fábrica, nada é instanciado ainda), e
// depois disso o supervisor que roda a "Thread da planta" e detecta se ela
// morre.
//
// NOTA (2026-07-30): a Thread do adaptador de rede (OPC-UA) e todo o
// catálogo de descoberta de Sensor/Actuator/Controller foram retirados
// daqui de propósito, pendentes de redesenho — `Sensor`/`Actuator` viraram
// traits mínimos (`sensor/mod.rs`, `actuator/mod.rs`), sem implementação
// concreta dentro de `monjolo` mais (isso agora é responsabilidade de quem
// monta a planta, ex. `tep-plant`). `Simulation` por enquanto só sabe rodar
// um `DynamicModel` — nenhum mecanismo de exposição externa existe ainda.
//
// Integrator (RK4): `tick_interval` é só o ritmo de parede (quanto a thread
// dorme entre rodadas) — nunca o passo físico de integração, que teria
// unidade errada (segundos de parede != horas de processo). `dt_hours` é o
// passo simulado de verdade, decidido à parte.
//
// Supervisor (lifecycle): a Thread da planta manda exatamente um
// `ServiceEvent` pro canal de lifecycle como último passo antes de
// retornar — seja por retorno normal, erro fatal sem pânico, ou pânico de
// verdade (capturado via `std::panic::catch_unwind`, nunca deixado vazar
// pra fora da thread). `run()` bloqueia em `events_rx.recv()`.

use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::dynamic_model::DynamicModel;
use crate::numerical_method::NumericalMethod;
use crate::state_registry::{Proxy, StateRegistry};

type ModelFactory = dyn FnOnce(&mut StateRegistry) -> (Box<dyn DynamicModel>, Vec<String>) + Send;

/** Evento de fim de vida da Thread da planta — manda exatamente um destes,
como último passo antes de retornar. `run()` bloqueia em `events_rx.recv()`
esperando ele — é assim que percebe a thread morta sem precisar de polling.
*/
enum ServiceEvent {
    /// Terminou sem erro — hoje a plant thread roda um `loop {}` sem
    /// break, então isso nunca acontece de verdade, mas o tipo comporta
    /// pra quando isso deixar de ser verdade.
    Stopped,
    /// Encerrou por um erro que o próprio serviço detectou e decidiu
    /// devolver como `Err` — não um pânico de linguagem.
    Failed(String),
    /// Entrou em pânico — capturado por `catch_unwind`, nunca deixado
    /// vazar pra fora da thread.
    Panicked(String),
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
    tick_interval: Duration,
    dt_hours: f64,
    numerical_method: NumericalMethod,
}

impl Default for Simulation {
    fn default() -> Self {
        Self {
            model_factory: None,
            tick_interval: Duration::from_millis(500),
            dt_hours: 1.0 / 3600.0,
            numerical_method: NumericalMethod::default(),
        }
    }
}

impl Simulation {
    pub fn new() -> Self {
        Self::default()
    }

    /** Passo físico simulado por tick, em horas — a unidade que o resto da
    física do TEP usa. Não confundir com `tick_interval` (ritmo de parede,
    `std::thread::sleep`): os dois são independentes de propósito — quão
    rápido a thread roda não deveria mudar quanto tempo de processo cada
    passo avança. Default: 1 segundo simulado por tick (1.0 / 3600.0 horas).
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

    /** Define a fábrica do modelo — chamada só depois, dentro da "Thread da
    planta", com o `StateRegistry` já criado nesse contexto. Ex.:
    `simulation.set_model(TennesseeEastmanModel::new)`.

    Também captura `model.state_keys()` — o que o próprio modelo declara
    como integrável (`DynamicModel`, default vazio) — enquanto o tipo ainda
    é `M` concreto, antes de virar `Box<dyn DynamicModel>` (que já não
    permite mais chamar métodos além do trait).
    */
    pub fn set_model<M>(&mut self, factory: impl FnOnce(&mut StateRegistry) -> M + Send + 'static)
    where
        M: DynamicModel + 'static,
    {
        self.model_factory = Some(Box::new(move |registry: &mut StateRegistry| {
            let model = factory(registry);
            let state_keys = model.state_keys();
            (Box::new(model) as Box<dyn DynamicModel>, state_keys)
        }));
    }

    /** Chamada terminal — consome a `Simulation` (builder) e sobe a
    "Thread da planta" (só se `set_model()` foi chamado; devolve `Err` sem
    subir thread nenhuma caso contrário).

    Bloqueia até a thread encerrar — normalmente, erro fatal ou pânico
    (capturado, nunca propagado como pânico de verdade). `Ok(())` só no
    caso raro de encerrar limpo; qualquer erro ou pânico vira `Err`
    descrevendo por quê.
    */
    pub fn run(mut self) -> Result<(), String> {
        let model_factory = self
            .model_factory
            .take()
            .ok_or_else(|| "run: nada configurado — chame set_model() antes".to_string())?;

        eprintln!(
            "[main] Simulation::run — método numérico: {:?}",
            self.numerical_method,
        );

        let tick_interval = self.tick_interval;
        let dt_hours = self.dt_hours;
        let numerical_method = self.numerical_method;

        let (events_tx, events_rx) = std::sync::mpsc::channel::<ServiceEvent>();

        let handle = Self::spawn_plant_thread(
            model_factory,
            tick_interval,
            dt_hours,
            numerical_method,
            events_tx,
        );

        let event = events_rx.recv().map_err(|_| {
            "run: a plant thread não reportou nada — canal de lifecycle fechado inesperadamente"
                .to_string()
        })?;

        // A thread já mandou seu evento — está a um passo de retornar (foi
        // o último passo antes disso). Juntar ela é rápido e seguro.
        let _ = handle.join();

        match event {
            ServiceEvent::Stopped => Ok(()),
            ServiceEvent::Failed(reason) => Err(format!("plant: encerrou com erro fatal: {reason}")),
            ServiceEvent::Panicked(reason) => Err(format!("plant: entrou em pânico: {reason}")),
        }
    }

    /** Sobe a "Thread da planta": cria `StateRegistry`, o modelo (nada
    disso existe antes desse ponto) e entra no loop de tick — integra via
    RK4 o que o modelo declarou em `state_keys()`, ou só avalia se não há
    nada pra integrar.

    O corpo inteiro roda dentro de `catch_unwind` — um pânico aqui (seja na
    inscrição inicial, seja em qualquer tick depois) nunca escapa da thread:
    vira um `ServiceEvent::Panicked` mandado pro canal de lifecycle.
    */
    fn spawn_plant_thread(
        model_factory: Box<ModelFactory>,
        tick_interval: Duration,
        dt_hours: f64,
        numerical_method: NumericalMethod,
        events: Sender<ServiceEvent>,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("plant".to_string())
            .spawn(move || {
                let outcome = panic::catch_unwind(AssertUnwindSafe(move || {
                    let registry = StateRegistry::shared();
                    let (model, model_state_keys) = model_factory(&mut registry.borrow_mut());

                    // Cada chave de estado integrável precisa de uma
                    // contraparte ".derivative" (seção 8.3 do plano) — pede
                    // as duas como `need` aqui, antes do resolve() geral,
                    // pra sair com Proxy pareado (estado, derivada) na
                    // mesma ordem de model_state_keys.
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

                    eprintln!(
                        "[plant] iniciando — {} chave(s) de estado integrável, tick a cada {tick_interval:?} (dt = {dt_hours}h)",
                        state_proxies.len(),
                    );

                    loop {
                        if state_proxies.is_empty() {
                            // Nenhum componente do modelo declarou
                            // state_keys() — não há nada pra integrar, só
                            // avalia a árvore uma vez (mesmo comportamento
                            // de antes do Integrator existir).
                            model.evaluate();
                        } else {
                            let current: Vec<f64> = state_proxies.iter().map(Proxy::get).collect();

                            // A closure é o "dynamics" da seção 9.6: escreve
                            // o estado perturbado (um k-ésimo sub-passo do
                            // RK4) nos Proxys de estado, dispara evaluate()
                            // da árvore inteira (que lê esse estado e
                            // recalcula tudo, inclusive as derivadas) e
                            // devolve as derivadas resultantes.
                            let next =
                                integrator.step(&current, dt_hours, &mut |perturbed: &[f64]| {
                                    for (proxy, &value) in state_proxies.iter().zip(perturbed) {
                                        proxy.set(value);
                                    }
                                    model.evaluate();
                                    derivative_proxies.iter().map(Proxy::get).collect()
                                });

                            // O último evaluate() acima rodou sobre s4 (um
                            // sub-passo hipotético do RK4, não o estado
                            // final combinado) — escreve o estado de
                            // verdade e reavalia mais uma vez pra
                            // EvaluationState refletir o que vai ser
                            // commitado, não o resíduo do último k4.
                            for (proxy, &value) in state_proxies.iter().zip(&next) {
                                proxy.set(value);
                            }
                            model.evaluate();
                        }

                        registry.borrow_mut().commit();

                        std::thread::sleep(tick_interval);
                    }
                }));

                let event = match outcome {
                    Ok(()) => ServiceEvent::Stopped,
                    Err(payload) => ServiceEvent::Panicked(panic_message(payload)),
                };
                let _ = events.send(event);
            })
            .expect("run: falha ao criar a thread da planta")
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
    fn run_requires_model() {
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

    /// dv/dt = -v, nasce em 100.0 — declara `state_keys()` (o que
    /// Valve/Agitator já fazem hoje). Guarda o último valor observado num
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
