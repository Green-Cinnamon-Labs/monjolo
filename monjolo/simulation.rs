/* monjolo/simulation.rs */

/** Interface externa do framework — a fachada/builder pública que quem monta uma planta (ex.:
TennesseeEastmanModel) usa pra rodar de verdade. Tudo em
dynamic_model.rs/state_registry.rs/numerical_method/actuator/sensor/disturbance é implementação
interna.

Simulation é o lifecycle manager do framework: um BUILDER até `run()` ser chamado (`set_model()` só
guarda a fábrica, nada é instanciado ainda), e depois disso o supervisor que roda a "Thread da
planta" e detecta se ela morre.

NOTA (2026-07-30): a Thread do adaptador de rede (OPC-UA) e todo o catálogo de descoberta de
Sensor/Actuator/Controller foram retirados daqui de propósito, pendentes de redesenho —
`Sensor`/`Actuator` viraram traits mínimos (`sensor/mod.rs`, `actuator/mod.rs`), sem implementação
concreta dentro de `monjolo` mais (isso agora é responsabilidade de quem monta a planta, ex.
`tep-plant`). `Simulation` por enquanto só sabe rodar um `DynamicModel` — nenhum mecanismo de
exposição externa existe ainda.

Integrator (RK4): `tick_interval` é só o ritmo de parede (quanto a thread dorme entre rodadas) —
nunca o passo físico de integração, que teria unidade errada (segundos de parede != horas de
processo). `dt_hours` é o passo simulado de verdade, decidido à parte.

Supervisor (lifecycle): a Thread da planta manda exatamente um `ServiceEvent` pro canal de lifecycle
como último passo antes de retornar — seja por retorno normal, erro fatal sem pânico, ou pânico de
verdade (capturado via `std::panic::catch_unwind`, nunca deixado vazar pra fora da thread). `run()`
bloqueia em `events_rx.recv()`.
*/

use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::adapter::AdapterConfig;
use crate::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
use crate::numerical_method::NumericalMethod;
use crate::snapshot::Snapshot;
use crate::state_registry::{Proxy, StateRegistry};

type ModelFactory =
    dyn FnOnce(&mut StateRegistry, &Snapshot) -> (Box<dyn DynamicModel>, Vec<String>) + Send;

/** Evento de fim de vida da Thread da planta — manda exatamente um destes, como último passo antes
de retornar. `run()` bloqueia em `events_rx.recv()` esperando ele — é assim que percebe a thread
morta sem precisar de polling.
*/
enum ServiceEvent {
    /* Terminou sem erro — hoje a plant thread roda um `loop {}` sem break, então isso nunca
    acontece de verdade, mas o tipo comporta pra quando isso deixar de ser verdade.
    */
    Stopped,
    /* Encerrou por um erro que o próprio serviço detectou e decidiu devolver como `Err` — não um
    pânico de linguagem.
    */
    Failed(String),
    /* Entrou em pânico — capturado por `catch_unwind`, nunca deixado vazar pra fora da thread. */
    Panicked(String),
}

/** Extrai uma mensagem legível do payload de um pânico capturado por `catch_unwind` —
`panic!("...")`/`panic!("{}", x)` produzem `&str` ou `String`; qualquer outro tipo (raro — ex.:
`panic_any` com um tipo próprio) cai no fallback.
*/
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
    config_path: Option<String>,
    tick_interval: Duration,
    dt_hours: f64,
    numerical_method: NumericalMethod,
    adapter: Option<AdapterConfig>,
}

impl Default for Simulation {
    fn default() -> Self {
        Self {
            model_factory: None,
            config_path: None,
            tick_interval: Duration::from_millis(500),
            dt_hours: 1.0 / 3600.0,
            numerical_method: NumericalMethod::default(),
            adapter: None,
        }
    }
}

impl Simulation {
    pub fn new() -> Self {
        Self::default()
    }

    /** Passo físico simulado por tick, em horas — a unidade que o resto da física do TEP usa. Não
    confundir com `tick_interval` (ritmo de parede, `std::thread::sleep`): os dois são independentes
    de propósito — quão rápido a thread roda não deveria mudar quanto tempo de processo cada passo
    avança. Default: 1 segundo simulado por tick (1.0 / 3600.0 horas).
    */
    pub fn set_dt_hours(&mut self, dt_hours: f64) {
        self.dt_hours = dt_hours;
    }

    /** Ritmo de parede entre rodadas (`std::thread::sleep`) — não tem relação com `dt_hours`, ver
    comentário no topo do arquivo. Default: 500ms.
    */
    pub fn set_tick_interval(&mut self, interval: Duration) {
        self.tick_interval = interval;
    }

    /** Escolhe o método numérico de integração — só aceita o que `NumericalMethod` (enum fechado,
    `numerical_method/mod.rs`) já implementa dentro do framework, nunca uma implementação arbitrária
    de fora. Default: `NumericalMethod::RK4`. `run()` consome isso via
    `NumericalMethod::integrator()` dentro da "Thread da planta".
    */
    pub fn set_numerical_method(&mut self, method: NumericalMethod) {
        self.numerical_method = method;
    }

    /** Liga um adaptador de rede (`adapter/mod.rs`) — hoje só `AdapterConfig::OpcUa`, e só existe
    uma variante construível com a feature `opcua` ligada (sem ela, `AdapterConfig` fica sem nenhum
    valor possível de passar aqui, mas a chamada continua compilando). Sobe numa thread própria,
    dentro de `spawn_plant_thread`, só depois do `StateRegistry` já ter resolvido tudo — nunca antes
    de existir isso pra descobrir.
    */
    pub fn set_adapter(&mut self, adapter: AdapterConfig) {
        self.adapter = Some(adapter);
    }

    /** Caminho do arquivo de configuração (condição inicial, análogo a `application.yaml` do
    Spring) — carregado UMA vez, dentro da "Thread da planta", antes de qualquer componente ser
    construído. `#[dynamic_model]` usa isso pra semear campos `#[config(...)]`
    (`monjolo::component`/`monjolo-macros/dynamic_model.rs`); quem chama `set_model()` também
    recebe a mesma referência (segundo parâmetro da fábrica), pro caso de ainda construir algo à
    mão que precise de condição inicial (ex.: `build_tep(registry, initial)`).

    Opcional: sem isso, `run()` usa um `Snapshot` vazio (`Snapshot::from_pairs(&[])` — toda chave
    de config vira 0.0, mesmo default que os slots já teriam).
    */
    pub fn set_config_path(&mut self, path: impl Into<String>) {
        self.config_path = Some(path.into());
    }

    /** Define a fábrica do modelo — chamada só depois, dentro da "Thread da planta", com o
    `StateRegistry` e o `Snapshot` de config já prontos nesse contexto. Ex.:
    `simulation.set_model(build_tep)` (`build_tep(&mut StateRegistry, &Snapshot) -> Composite` já
    bate com a assinatura direto, sem precisar de closure).

    Opcional: nada obriga a chamar isto — se todo componente da planta for declarado via
    `#[dynamic_model]`/`#[actuator(...)]`/etc., `run()` monta a simulação inteira só a partir do
    que `inventory` descobre, sem nenhum modelo construído à mão.

    O `model` que `factory` devolve (quando chamado) nunca é o que roda sozinho: vira o PRIMEIRO
    filho de uma `Composite` interna (`root`) que esta função monta — logo depois, varre
    `inventory::iter::<ComponentDescriptor>()` (tudo que `#[actuator(...)]` e macros irmãs
    registraram escondido) e anexa cada componente descoberto a `root`, em fase fixa: (A)
    `Dynamic` primeiro — mesma fase de `model` (subsistemas com ordem obrigatória entre si, ex.
    Reactor antes de Compressor, continuam construídos à mão dentro de `factory` por causa disso;
    `inventory::iter` não garante ordem nenhuma) —, depois (B) `Actuator`, depois (C) `Controller`.
    `Sensor` nunca entra aqui: seu `construct` sempre devolve `None` (não é `DynamicModel`, é lido
    sob demanda, não avaliado por tick). Cada `construct` roda exatamente uma vez, aqui — a
    instância que resulta é a mesma que vive pelo resto da simulação.

    `state_keys()` só é capturado DEPOIS de `root` estar completa (modelo manual + descobertos):
    `Composite::state_keys()` agrega os filhos recursivamente (dynamic_model.rs), e só agora existe
    algo pra agregar — antes desta função montar `root`, um `Actuator` nunca chegava a ser somado
    (nem existia ainda, na verdade: quem o descobre é este método).
    */
    pub fn set_model<M>(
        &mut self,
        factory: impl FnOnce(&mut StateRegistry, &Snapshot) -> M + Send + 'static,
    ) where
        M: DynamicModel + 'static,
    {
        self.model_factory = Some(Box::new(move |registry: &mut StateRegistry, config: &Snapshot| {
            let model = factory(registry, config);

            let mut root = Composite::new();
            root.add_dynamic(Box::new(model));
            crate::component::attach_discovered_components(&mut root, registry, config);

            let state_keys = root.state_keys();
            (Box::new(root) as Box<dyn DynamicModel>, state_keys)
        }));
    }

    /** Chamada terminal — consome a `Simulation` (builder) e sobe a "Thread da planta". Devolve
    `Err` sem subir thread nenhuma se NEM `set_model()` NEM `set_config_path()` foram chamados —
    não dá pra saber se isso foi esquecido ou se é mesmo pra rodar vazio; exigir pelo menos um dos
    dois é o sinal mínimo de "sim, quero rodar algo". Se só `set_model()` foi chamado, `run()` usa
    `Snapshot` vazio pra config; se só `set_config_path()`, a simulação é inteiramente montada por
    `inventory` (nenhum modelo construído à mão).

    Bloqueia até a thread encerrar — normalmente, erro fatal ou pânico (capturado, nunca propagado
    como pânico de verdade). `Ok(())` só no caso raro de encerrar limpo; qualquer erro ou pânico
    vira `Err` descrevendo por quê.
    */
    pub fn run(mut self) -> Result<(), String> {
        if self.model_factory.is_none() && self.config_path.is_none() {
            return Err(
                "run: nada configurado — chame set_model() e/ou set_config_path() antes".to_string(),
            );
        }

        /* Sem set_model(): a simulação é inteiramente montada por descoberta — mesma lógica de
        set_model(), só sem nenhum modelo manual como primeiro filho de `root`.
        */
        let model_factory = self.model_factory.take().unwrap_or_else(|| {
            Box::new(move |registry: &mut StateRegistry, config: &Snapshot| {
                let mut root = Composite::new();
                crate::component::attach_discovered_components(&mut root, registry, config);
                let state_keys = root.state_keys();
                (Box::new(root) as Box<dyn DynamicModel>, state_keys)
            })
        });

        eprintln!(
            "[main] Simulation::run — método numérico: {:?}",
            self.numerical_method,
        );

        let tick_interval = self.tick_interval;
        let dt_hours = self.dt_hours;
        let numerical_method = self.numerical_method;
        let config_path = self.config_path.take();
        let adapter = self.adapter.take();

        let (events_tx, events_rx) = std::sync::mpsc::channel::<ServiceEvent>();

        let handle = Self::spawn_plant_thread(
            model_factory,
            config_path,
            tick_interval,
            dt_hours,
            numerical_method,
            adapter,
            events_tx,
        );

        let event = events_rx.recv().map_err(|_| {
            "run: a plant thread não reportou nada — canal de lifecycle fechado inesperadamente"
                .to_string()
        })?;

        /* A thread já mandou seu evento — está a um passo de retornar (foi o último passo antes
        disso). Juntar ela é rápido e seguro.
        */
        let _ = handle.join();

        match event {
            ServiceEvent::Stopped => Ok(()),
            ServiceEvent::Failed(reason) => Err(format!("plant: encerrou com erro fatal: {reason}")),
            ServiceEvent::Panicked(reason) => Err(format!("plant: entrou em pânico: {reason}")),
        }
    }

    /** Sobe a "Thread da planta": cria `StateRegistry`, carrega o `Snapshot` de config (se
    `set_config_path()` foi chamado — senão, vazio), o modelo (nada disso existe antes desse
    ponto) e entra no loop de tick — integra via RK4 o que o modelo declarou em `state_keys()`, ou
    só avalia se não há nada pra integrar.

    O corpo inteiro roda dentro de `catch_unwind` — um pânico aqui (seja carregando config, na
    inscrição inicial, seja em qualquer tick depois) nunca escapa da thread: vira um
    `ServiceEvent::Panicked` mandado pro canal de lifecycle.
    */
    fn spawn_plant_thread(
        model_factory: Box<ModelFactory>,
        config_path: Option<String>,
        tick_interval: Duration,
        dt_hours: f64,
        numerical_method: NumericalMethod,
        adapter: Option<AdapterConfig>,
        events: Sender<ServiceEvent>,
    ) -> JoinHandle<()> {
        std::thread::Builder::new()
            .name("plant".to_string())
            .spawn(move || {
                let outcome = panic::catch_unwind(AssertUnwindSafe(move || {
                    let config = match &config_path {
                        Some(path) => Snapshot::from_file(path).unwrap_or_else(|err| {
                            panic!("plant thread: falha ao carregar config de '{path}': {err}")
                        }),
                        None => Snapshot::from_pairs(&[]),
                    };

                    let registry = StateRegistry::shared();
                    let (model, model_state_keys) =
                        model_factory(&mut registry.borrow_mut(), &config);

                    /* Cada chave de estado integrável precisa de uma contraparte ".derivative"
                    (seção 8.3 do plano) — pede as duas como `need` aqui, antes do resolve() geral,
                    pra sair com Proxy pareado (estado, derivada) na mesma ordem de
                    model_state_keys.
                    */
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

                    /* Sobe a thread do adaptador (hoje só OPC-UA) só depois do resolve() acima —
                    StateRegistry/sensor_catalog/actuator_catalog só estão completos e estáveis a
                    partir daqui. `Rc`/`Proxy`/`StateRegistry` nunca saem desta thread: só o que
                    atravessa é `Arc<dyn Sensor>` (já Send+Sync de verdade) e os NOMES dos atuadores
                    — a escrita em si volta por `command_rx`, drenado a cada tick do loop abaixo,
                    nunca dentro da thread do adaptador (ver `monjolo::adapter::opcua`).
                    */
                    let _ = &adapter;
                    #[cfg(feature = "opcua")]
                    let command_rx: Option<std::sync::mpsc::Receiver<(String, f64)>> =
                        Self::spawn_adapter_thread(adapter, &registry);
                    #[cfg(not(feature = "opcua"))]
                    let command_rx: Option<std::sync::mpsc::Receiver<(String, f64)>> = None;

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
                        /* Drena os comandos de escrita que chegaram pela thread do adaptador desde
                        o último tick (ver comentário acima do `spawn_adapter_thread`) — ponto
                        único e determinístico de aplicação, sempre antes da física deste tick.
                        `Rc<dyn Actuator>` nunca sai desta thread: só o nome/valor atravessou.
                        */
                        if let Some(rx) = &command_rx {
                            while let Ok((name, value)) = rx.try_recv() {
                                match registry.borrow().actuator(&name) {
                                    Some(actuator) => {
                                        actuator.write(value);
                                        eprintln!("[adapter] escrita aplicada: {name} = {value}");
                                    }
                                    None => eprintln!(
                                        "[adapter] escrita ignorada — atuador \"{name}\" não catalogado"
                                    ),
                                }
                            }
                        }

                        if state_proxies.is_empty() {
                            /* Nenhum componente do modelo declarou state_keys() — não há nada pra
                            integrar, só avalia a árvore uma vez (mesmo comportamento de antes do
                            Integrator existir).
                            */
                            model.evaluate();
                        } else {
                            let current: Vec<f64> = state_proxies.iter().map(Proxy::get).collect();

                            /* A closure é o "dynamics" da seção 9.6: escreve o estado perturbado
                            (um k-ésimo sub-passo do RK4) nos Proxys de estado, dispara evaluate()
                            da árvore inteira (que lê esse estado e recalcula tudo, inclusive as
                            derivadas) e devolve as derivadas resultantes.
                            */
                            let next =
                                integrator.step(&current, dt_hours, &mut |perturbed: &[f64]| {
                                    for (proxy, &value) in state_proxies.iter().zip(perturbed) {
                                        proxy.set(value);
                                    }
                                    model.evaluate();
                                    derivative_proxies.iter().map(Proxy::get).collect()
                                });

                            /* O último evaluate() acima rodou sobre s4 (um sub-passo hipotético do
                            RK4, não o estado final combinado) — escreve o estado de verdade e
                            reavalia mais uma vez pra EvaluationState refletir o que vai ser
                            commitado, não o resíduo do último k4.
                            */
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

    /** Sobe a thread do adaptador de rede, se `adapter` foi configurado — hoje só
    `AdapterConfig::OpcUa`. Roda num runtime tokio `current_thread` próprio (sem pool de worker
    threads — não há trabalho paralelo real a justificar um, ver `adapter/opcua.rs`).

    Só `Arc<dyn Sensor>` (catalogado, já Send+Sync de verdade) atravessa pra essa thread — nenhum
    `Rc`/`Proxy`/`StateRegistry` sai daqui. Devolve o lado de leitura de um canal `(nome, valor)`:
    escrita de atuador nunca acontece nesta thread, só é anunciada por ela — quem aplica de
    verdade é `spawn_plant_thread`, drenando esse canal a cada tick.

    `actuators`: cada atuador ganha um `Sensor` "espelho" só-leitura na MESMA chave (`Sensor` nunca
    inventa valor próprio, só lê de volta um `#[state]`/`#[offer]` que já existe — a própria posição
    do atuador, exatamente como `ReactorPressure` lê `reactor.temperature`) — sem isso, `serve()`
    não tinha como publicar a posição de volta pro cliente OPC-UA: só existia o write callback
    (comando entrando), o node ficava travado no `0.0` inicial pra sempre, nunca refletindo o
    estado de verdade. Construído (e resolvido de novo) AQUI, depois do resolve() geral em
    `spawn_plant_thread` — sensores novos precisam de outro `resolve()` antes de `read()` ser
    seguro (mesmo ciclo declare → resolve de qualquer `Sensor`).

    Erro do servidor OPC-UA (`serve()` retornando `Err`, ex.: porta ocupada) só é logado — não
    propaga pro `ServiceEvent` do supervisor nesta primeira versão; simplificação deliberada, não um
    esquecimento (a Thread da planta continua rodando normalmente mesmo se o adaptador cair).
    */
    #[cfg(feature = "opcua")]
    fn spawn_adapter_thread(
        adapter: Option<AdapterConfig>,
        registry: &std::rc::Rc<std::cell::RefCell<StateRegistry>>,
    ) -> Option<std::sync::mpsc::Receiver<(String, f64)>> {
        let AdapterConfig::OpcUa { endpoint } = adapter?;

        let sensors: std::collections::HashMap<String, std::sync::Arc<dyn crate::sensor::Sensor>> =
            registry
                .borrow()
                .sensor_names()
                .map(|name| {
                    let sensor = registry
                        .borrow()
                        .sensor(name)
                        .expect("sensor_names() e sensor() devem concordar sobre o catálogo");
                    (name.to_string(), sensor)
                })
                .collect();

        let actuator_names: Vec<String> =
            registry.borrow().actuator_names().map(String::from).collect();
        let actuators: std::collections::HashMap<String, std::sync::Arc<dyn crate::sensor::Sensor>> =
            actuator_names
                .iter()
                .map(|name| {
                    let shadow = crate::sensor::model::Sensor::new(
                        &mut registry.borrow_mut(),
                        name,
                        Box::new(crate::sensor::model::Ideal),
                    );
                    (name.clone(), shadow as std::sync::Arc<dyn crate::sensor::Sensor>)
                })
                .collect();
        registry
            .borrow_mut()
            .resolve()
            .expect("adapter thread: falha ao resolver os sensores-espelho dos atuadores");

        let (command_tx, command_rx) = std::sync::mpsc::channel::<(String, f64)>();

        std::thread::Builder::new()
            .name("adapter".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("adapter thread: falha ao criar runtime tokio");

                let outcome = runtime.block_on(crate::adapter::opcua::serve(
                    sensors,
                    actuators,
                    command_tx,
                    &endpoint,
                ));
                if let Err(err) = outcome {
                    eprintln!("[adapter] servidor OPC-UA encerrou com erro: {err}");
                }
            })
            .expect("run: falha ao criar a thread do adapter");

        Some(command_rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /* Modelo mínimo só pra provar que `run()` tica de verdade — não tem estado no StateRegistry
    nenhum, só conta quantas vezes `evaluate()` foi chamado.
    */
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
        /* Arc<AtomicUsize> é Send — atravessa a fronteira dentro de set_model mesmo o CountingModel
        resultante não sendo Send.
        */
        simulation.set_model(move |_registry, _config| CountingModel {
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

    /* dv/dt = -v, nasce em 100.0 — declara `state_keys()` (o que Valve/Agitator já fazem hoje).
    Guarda o último valor observado num Arc<Mutex<f64>> pra provar, de fora da thread da planta, que
    run() está mesmo chamando o Integrator a cada tick.
    */
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
        simulation.set_model(move |registry, _config| DecayModel::new(registry, observed_for_build.clone()));

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

    /* Modelo que entra em pânico depois de alguns ticks saudáveis — simula uma falha real dentro de
    evaluate(). Prova o supervisor inteiro: catch_unwind captura o pânico dentro da plant thread,
    vira ServiceEvent::Panicked, e run() RETORNA (em vez de travar pra sempre, que era o
    comportamento de qualquer pânico não capturado numa thread sem ninguém dando join nela).
    */
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
        simulation.set_model(move |_registry, _config| PanickyModel {
            ticks: ticks.clone(),
        });

        /* Chamado direto (sem thread própria de teste) — se o supervisor não funcionasse, isso
        travaria o teste pra sempre em vez de devolver um Err.
        */
        let result = simulation.run();

        let message = result.expect_err("esperava Err depois do pânico da PanickyModel");
        assert!(
            message.contains("pane proposital"),
            "mensagem inesperada: {message}"
        );
    }
}
