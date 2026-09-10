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

NOTA (2026-09-09, issue #67): adaptador de rede saiu de vez daqui, de novo — desta vez não "ainda não
redesenhado", mas por decisão de arquitetura definitiva: `Simulation` não deve saber que um adaptador
existe. Quem sobe/gerencia um adaptador é `crate::runtime::Runtime`, um objeto persistente que
sobrevive a várias `Simulation`s ao longo do tempo (cada `reset()` descarta a atual e sobe outra do
zero) — ver `spec-tennessee-eastman/docs/issue61_runtime_supervisor/nota_runtime_supervisor.md` pro
desenho completo. `run()` (bloqueante, API antiga) e `spawn()` (não-bloqueante, o que `Runtime` usa)
coexistem: `run()` agora é só `self.spawn()?.wait()`.

Integrator (RK4): `tick_interval` é só o ritmo de parede (quanto a thread dorme entre rodadas) —
nunca o passo físico de integração, que teria unidade errada (segundos de parede != horas de
processo). `dt_hours` é o passo simulado de verdade, decidido à parte.

Supervisor (lifecycle): a Thread da planta manda exatamente um `ServiceEvent` pro canal de lifecycle
como último passo antes de retornar — seja por retorno normal (inclusive um `reset()` do `Runtime`,
que agora é a forma normal de terminar, não só um caso de borda nunca exercitado), erro fatal sem
pânico, ou pânico de verdade (capturado via `std::panic::catch_unwind`, nunca deixado vazar pra fora
da thread). `RunningSimulation::wait()` bloqueia em `events_rx.recv()` (`run()` só chama isso).
*/

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
use crate::numerical_method::NumericalMethod;
use crate::runtime_control::RuntimeControl;
use crate::sensor::Sensor;
use crate::snapshot::Snapshot;
use crate::state_registry::{Proxy, StateRegistry};

type ModelFactory =
    dyn FnOnce(&mut StateRegistry, &Snapshot) -> (Box<dyn DynamicModel>, Vec<String>) + Send;

/** Tudo que um adaptador externo precisa pra expor UMA instância de planta, empacotado como uma
única unidade — o que `Runtime` (`runtime.rs`) troca atomicamente a cada `reset()`, via
`RwLock<Arc<PlantBinding>>`, nunca campo por campo (ver "por que trocar tudo de uma vez, num Arc só"
na nota_runtime_supervisor.md — 4 peças trocadas independentemente deixa uma janela onde um leitor
pega sensores NOVOS com o `commands` VELHO, cujo receptor já morreu junto da Thread da planta antiga).

Mesmo conteúdo que `spawn_adapter_thread` construía ad-hoc antes de #67 existir — só que agora
nomeado e reenviado pra fora da Thread da planta via `ready`, em vez de consumido ali mesmo pra subir
um servidor OPC-UA direto. Não sabe nada de OPC-UA: é o mesmo tipo que serviria qualquer adaptador
futuro (MQTT, REST, etc.).
*/
pub struct PlantBinding {
    pub sensors: HashMap<String, Arc<dyn Sensor>>,
    /* "Sensor" espelho de cada atuador — mesma técnica de sempre (Art. 3.6.6/12.1 do CONTRIBUTING):
    lê de volta a própria posição do atuador, que já é Send+Sync via StateRegistry. */
    pub actuators: HashMap<String, Arc<dyn Sensor>>,
    pub commands: Sender<(String, f64)>,
    pub control: Arc<RuntimeControl>,
}

/** Evento de fim de vida da Thread da planta — manda exatamente um destes, como último passo antes
de retornar. `RunningSimulation::wait()` bloqueia em `events_rx.recv()` esperando ele — é assim que
percebe a thread morta sem precisar de polling.
*/
enum ServiceEvent {
    /* Terminou sem erro — antes de #67 a plant thread rodava um `loop {}` sem break, então isso
    nunca acontecia de verdade; agora é o caminho NORMAL de término, tomado quando
    `RuntimeControl::take_reset_request()` devolve `true` (`Runtime::reset()`/`shutdown()`).
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

/** Devolvido por `Simulation::spawn()` — a Thread da planta já está rodando (ou pelo menos já foi
criada; `ready` ainda pode não ter chegado). Quem segura isto decide o QUANDO: pode esperar `ready`
pra saber que o `StateRegistry` resolveu e os sensores/atuadores-espelho existem, pode chamar
`control` a qualquer momento (não depende de `ready`, já que `RuntimeControl` é criado ANTES da
thread subir), e decide se/quando chama `wait()` pra bloquear até ela morrer.
*/
pub struct RunningSimulation {
    pub handle: JoinHandle<()>,
    pub control: Arc<RuntimeControl>,
    /** Recebe exatamente UM `PlantBinding`, assim que `StateRegistry::resolve()` (geral +
    sensores-espelho de atuador) terminar dentro da Thread da planta. Nunca manda um segundo —
    quem quiser saber sensores/atuadores de novo depois disso já tem o `Arc<dyn Sensor>` (Send+Sync
    de verdade, lido quantas vezes quiser via `.read()`), não precisa reconsultar este canal.
    */
    pub ready: Receiver<PlantBinding>,
    events: Receiver<ServiceEvent>,
}

impl RunningSimulation {
    /** Bloqueia até a Thread da planta encerrar — normalmente (hoje: só depois de um `reset()`/
    `shutdown()` externo pedir isso via `RuntimeControl`), erro fatal, ou pânico — e junta a thread.
    Mesmo comportamento terminal que `Simulation::run()` sempre teve (`run()` é literalmente
    `self.spawn()?.wait()` agora), só exposto separadamente aqui pra quem (ex.: `crate::runtime::
    Runtime`) precisa fazer outra coisa com o `RunningSimulation` (ler `control`/`ready`) ANTES de
    bloquear nisto.
    */
    pub fn wait(self) -> Result<(), String> {
        let event = self.events.recv().map_err(|_| {
            "wait: a plant thread não reportou nada — canal de lifecycle fechado inesperadamente"
                .to_string()
        })?;

        /* A thread já mandou seu evento — está a um passo de retornar (foi o último passo antes
        disso). Juntar ela é rápido e seguro.
        */
        let _ = self.handle.join();

        match event {
            ServiceEvent::Stopped => Ok(()),
            ServiceEvent::Failed(reason) => Err(format!("plant: encerrou com erro fatal: {reason}")),
            ServiceEvent::Panicked(reason) => Err(format!("plant: entrou em pânico: {reason}")),
        }
    }
}

pub struct Simulation {
    model_factory: Option<Box<ModelFactory>>,
    config_path: Option<String>,
    tick_interval: Duration,
    dt_hours: f64,
    numerical_method: NumericalMethod,
    runtime_control: Arc<RuntimeControl>,
}

impl Default for Simulation {
    fn default() -> Self {
        Self {
            model_factory: None,
            config_path: None,
            tick_interval: Duration::from_millis(500),
            dt_hours: 1.0 / 3600.0,
            numerical_method: NumericalMethod::default(),
            runtime_control: Arc::new(RuntimeControl::new()),
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

    /** Devolve um clone do `Arc<RuntimeControl>` desta `Simulation` — chame ANTES de `run()`/
    `spawn()` (que consomem `self` por valor): a mesma instância acompanha a Thread da planta por
    dentro (lida a cada tick), e pausar/mudar velocidade por este handle é visível por ela
    imediatamente, mesmo `Arc`. `spawn()` também devolve o mesmo `Arc` em
    `RunningSimulation::control` — chamar este método antes é só pra quem precisa dele ANTES de
    `spawn()` retornar (ex.: `Runtime::new()`, que constrói a `Simulation` e precisa decidir o que
    fazer com o controle antes mesmo de ela terminar de subir).
    */
    pub fn runtime_control(&self) -> Arc<RuntimeControl> {
        self.runtime_control.clone()
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

    /** Chamada terminal não-bloqueante — consome a `Simulation` (builder), sobe a "Thread da
    planta" e devolve IMEDIATAMENTE um `RunningSimulation` (handle da thread + `RuntimeControl` +
    o lado de leitura de dois canais: `ready`, que recebe exatamente um `PlantBinding` assim que o
    `StateRegistry` resolver e os sensores/atuadores-espelho estiverem prontos, e o canal de
    lifecycle interno que `RunningSimulation::wait()` consome). Quem só quer "rodar e bloquear até
    acabar", como sempre foi o comportamento de `Simulation`, continua usando `run()` — que agora é
    só `self.spawn()?.wait()`. Quem precisa de mais controle sobre o QUANDO (ex.: `crate::runtime::
    Runtime`, que precisa saber quando a planta está pronta pra trocar seu `PlantBinding`, e decidir
    depois quando/se espera ela morrer) usa `spawn()` direto.

    Mesma validação de sempre: `Err` sem subir thread nenhuma se NEM `set_model()` NEM
    `set_config_path()` foram chamados.
    */
    pub fn spawn(mut self) -> Result<RunningSimulation, String> {
        if self.model_factory.is_none() && self.config_path.is_none() {
            return Err(
                "spawn: nada configurado — chame set_model() e/ou set_config_path() antes".to_string(),
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
            "[main] Simulation::spawn — método numérico: {:?}",
            self.numerical_method,
        );

        let tick_interval = self.tick_interval;
        let dt_hours = self.dt_hours;
        let numerical_method = self.numerical_method;
        let config_path = self.config_path.take();
        let runtime_control = self.runtime_control.clone();

        let (events_tx, events_rx) = std::sync::mpsc::channel::<ServiceEvent>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<PlantBinding>();

        let handle = Self::spawn_plant_thread(
            model_factory,
            config_path,
            tick_interval,
            dt_hours,
            numerical_method,
            runtime_control.clone(),
            ready_tx,
            events_tx,
        );

        Ok(RunningSimulation {
            handle,
            control: runtime_control,
            ready: ready_rx,
            events: events_rx,
        })
    }

    /** Chamada terminal bloqueante — `self.spawn()?.wait()`. Mantido pelo comportamento histórico
    de `Simulation` (e por todo teste que já assumia isso, ver módulo `tests` embaixo): sobe a
    Thread da planta e só retorna quando ela encerra, erro fatal ou pânico (capturado, nunca
    propagado como pânico de verdade) virando `Err`; encerramento limpo (hoje: só via
    `crate::runtime::Runtime::reset()`/`shutdown()` pedindo à `Thread da planta pra parar) vira
    `Ok(())`.
    */
    pub fn run(self) -> Result<(), String> {
        self.spawn()?.wait()
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
        runtime_control: Arc<RuntimeControl>,
        ready: Sender<PlantBinding>,
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

                    /* Monta o PlantBinding e manda pra fora via `ready` só depois do resolve()
                    acima — StateRegistry/sensor_catalog/actuator_catalog só estão completos e
                    estáveis a partir daqui. `Rc`/`Proxy`/`StateRegistry` nunca saem desta thread: só
                    o que atravessa é `Arc<dyn Sensor>` (já Send+Sync de verdade) dentro do
                    `PlantBinding` — a escrita em si volta por `command_rx`, drenado a cada tick do
                    loop abaixo, nunca por quem recebe o `PlantBinding` (ver `crate::runtime::
                    Runtime`, `monjolo::adapter::opcua`). Construído incondicionalmente (não só sob a
                    feature `opcua`): o custo é uma dúzia de `HashMap`/`Arc::clone`, e não amarra mais
                    este arquivo a saber se ALGUÉM vai ler `ready` — quem não lê (ex.: os testes deste
                    módulo, que só usam `run()`) simplesmente deixa o `Sender` cair no chão.

                    `actuators`: cada atuador ganha um `Sensor` "espelho" só-leitura na MESMA chave
                    (`Sensor` nunca inventa valor próprio, só lê de volta um `#[state]`/`#[offer]`
                    que já existe — a própria posição do atuador) — sem isso, não haveria como
                    publicar a posição de volta pro cliente externo: só existiria o write callback
                    (comando entrando), o valor ficaria travado no `0.0` inicial pra sempre, nunca
                    refletindo o estado de verdade.
                    */
                    let sensors: HashMap<String, Arc<dyn Sensor>> = registry
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
                    let actuators: HashMap<String, Arc<dyn Sensor>> = actuator_names
                        .iter()
                        .map(|name| {
                            let shadow = crate::sensor::model::Sensor::new(
                                &mut registry.borrow_mut(),
                                name,
                                Box::new(crate::sensor::model::Ideal),
                            );
                            (name.clone(), shadow as Arc<dyn Sensor>)
                        })
                        .collect();
                    registry
                        .borrow_mut()
                        .resolve()
                        .expect("plant thread: falha ao resolver os sensores-espelho dos atuadores");

                    let (command_tx, command_rx) = std::sync::mpsc::channel::<(String, f64)>();
                    let _ = ready.send(PlantBinding {
                        sensors,
                        actuators,
                        commands: command_tx,
                        control: runtime_control.clone(),
                    });

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
                        /* Drena os comandos de escrita que chegaram de fora desde o último tick —
                        ponto único e determinístico de aplicação, sempre antes da física deste
                        tick. `Rc<dyn Actuator>` nunca sai desta thread: só o nome/valor atravessou.
                        Drenado mesmo pausado — um comando escrito durante a pausa já fica aplicado
                        pra quando a simulação retomar, em vez de se perder. Sempre um `Receiver`
                        de verdade agora (não mais `Option`): não custa nada drenar um canal do qual
                        ninguém nunca escreveu, `try_recv()` só devolve `Empty` na hora.
                        */
                        while let Ok((name, value)) = command_rx.try_recv() {
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

                        /* Pedido de reset (`Runtime::reset()`/`shutdown()`, via
                        `RuntimeControl::request_reset()`) encerra o loop de vez — a thread retorna
                        normalmente, `ServiceEvent::Stopped` é mandado, e quem construiu esta
                        `Simulation` (hoje: `crate::runtime::Runtime`) já está esperando nisso pra
                        saber que pode descartar `model`/`registry`/tudo aqui dentro (todos morrem
                        com a thread) e subir uma planta nova. Checado ANTES do `is_paused()` de
                        propósito: pedir reset enquanto pausado não pode ficar preso esperando um
                        `resume()` que talvez nunca venha.
                        */
                        if runtime_control.take_reset_request() {
                            eprintln!("[plant] reset solicitado — encerrando esta instância");
                            break;
                        }

                        /* Fator de velocidade escala só o ritmo de PAREDE (`tick_interval`) — nunca
                        `dt_hours` (Art. 1 do topo do arquivo: os dois são independentes de
                        propósito). 0.0 = o mais rápido possível (sem dormir); negativo já vira 0.0
                        dentro de `RuntimeControl::set_speed`.
                        */
                        let speed = runtime_control.speed();
                        let effective_interval = if speed <= 0.0 {
                            Duration::ZERO
                        } else {
                            tick_interval.div_f64(speed)
                        };

                        if runtime_control.is_paused() {
                            /* Pausado: física congelada — nem evaluate() roda, nem commit(), nem
                            t_h avança. Só dorme e tenta de novo — comandos de atuador continuam
                            sendo drenados acima, então retomar já aplica o que chegou entretanto.
                            */
                            std::thread::sleep(tick_interval);
                            continue;
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
                        runtime_control.advance_t_h(dt_hours);

                        std::thread::sleep(effective_interval);
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
