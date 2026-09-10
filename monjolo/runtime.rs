/* monjolo/runtime.rs */

/** O supervisor persistente que faltava — issue #67, desenho completo em
`spec-tennessee-eastman/docs/issue61_runtime_supervisor/nota_runtime_supervisor.md`. `Simulation`
(`simulation.rs`) é o builder/runner de UMA física, do início até alguém pedir pra parar; `Runtime` é
quem vive o processo inteiro, é dono do adaptador de rede (hoje só OPC-UA) pelo tempo de vida inteiro
dele, e troca atomicamente pra qual `Simulation` o adaptador aponta a cada `reset()` — o adaptador,
a conexão TCP de um cliente conectado, e a estrutura de nodes nunca caem.

Analogia usada na nota: isto é o mesmo movimento que fechar e dar `refresh()` num `ApplicationContext`
do Spring — o grafo de objetos inteiro (StateRegistry, Sensor/Actuator/Controller) é jogado fora e
reconstruído do zero a cada reset, nunca remendado no lugar.
*/

use std::sync::{Arc, Condvar, Mutex, RwLock};

use crate::simulation::{PlantBinding, RunningSimulation, Simulation};

/** Fábrica de uma `Simulation` nova — chamada uma vez no boot do `Runtime` e de novo a cada
`reset()`. Precisa ser `Fn`, não `FnOnce`: cada chamada tem que produzir uma `Simulation` igualmente
válida, sem depender de nada "gasto" na primeira vez. Na prática isso nunca exige um `model_factory`
reutilizável dentro de `Simulation` — quem escreve esta closure só chama `Simulation::new()` +
`set_model(...)`/`set_config_path(...)` de novo a cada vez, exatamente como faria uma segunda vez à
mão; `Simulation::set_model` continua aceitando `FnOnce` sem problema, porque cada `Simulation` que
sai daqui só é rodada (`spawn()`) uma única vez antes de ser descartada.
*/
type SimulationFactory = dyn Fn() -> Simulation + Send + Sync;

pub struct Runtime {
    build_simulation: Box<SimulationFactory>,
    binding: RwLock<Arc<PlantBinding>>,
    /** `None` só durante a janela interna de `reset()`/`request_shutdown()` entre "a antiga já foi
    `wait()`ada" e "a nova já foi `spawn()`ada" — nunca observável de fora: `current` continua
    trancado (mutex) o tempo todo dentro dessas chamadas, então ninguém mais consegue chamar
    `reset()`/`request_shutdown()` concorrentemente e ver este `None`.
    */
    current: Mutex<Option<RunningSimulation>>,
    shutdown: (Mutex<bool>, Condvar),
}

impl Runtime {
    /** Sobe a primeira `Simulation` (via `build_simulation()`), bloqueia até ela sinalizar pronta
    (`StateRegistry` resolvido, sensores/atuadores-espelho catalogados) e devolve um `Runtime` já
    operante — pronto pra subir um adapter em cima (`spawn_opcua_adapter`) ou ser consultado direto
    (`binding()`) em testes, sem nenhum adapter.

    `Arc<Self>`, não `Self`: todo mundo que precisa chamar `reset()`/`shutdown()`/`binding()` de fora
    (um Method do adaptador, `tep-plant/src/main.rs`) precisa da MESMA instância compartilhada, nunca
    uma cópia — `Runtime` não tem "duas fontes da verdade" sobre qual `PlantBinding` é o atual.
    */
    pub fn new(
        build_simulation: impl Fn() -> Simulation + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        let build_simulation: Box<SimulationFactory> = Box::new(build_simulation);
        let running = (build_simulation)().spawn()?;
        let initial_binding = running.ready.recv().map_err(|_| {
            "Runtime::new: a plant thread encerrou antes de sinalizar pronta".to_string()
        })?;

        Ok(Arc::new(Self {
            build_simulation,
            binding: RwLock::new(Arc::new(initial_binding)),
            current: Mutex::new(Some(running)),
            shutdown: (Mutex::new(false), Condvar::new()),
        }))
    }

    /** Devolve um clone do `Arc<PlantBinding>` da planta ATUAL — o que um adaptador (`adapter/
    opcua.rs`) lê a cada tick/callback. Nunca guarde isto por muito tempo: cada chamada pega um
    clone fresco do que está publicado agora, pra sempre enxergar a instância mais recente depois
    de um `reset()`.
    */
    pub fn binding(&self) -> Arc<PlantBinding> {
        self.binding
            .read()
            .expect("Runtime: RwLock<PlantBinding> envenenado")
            .clone()
    }

    /** Encerra a `Simulation` ATUAL (pede o stop cooperativo via `RuntimeControl::request_reset()`
    e BLOQUEIA até a Thread da planta antiga realmente morrer — `RunningSimulation::wait()`, join
    incluso), constrói e sobe uma nova do zero (`build_simulation()` de novo), espera ela sinalizar
    pronta, e só ENTÃO troca o `Arc<PlantBinding>` que `binding()` devolve — nunca as duas coisas ao
    mesmo tempo (ver "por que trocar tudo de uma vez, num Arc só" na nota_runtime_supervisor.md).

    Bloqueante: quem chama de um contexto que não pode travar (ex.: o callback de um Method OPC-UA,
    rodando num runtime tokio de thread única) precisa chamar isto de dentro do seu próprio
    `std::thread::spawn`, não direto no callback — `Runtime` não faz essa cortesia sozinho, porque
    nem todo chamador precisa dela (um teste chamando `reset()` direto QUER o bloqueio).
    */
    pub fn reset(&self) -> Result<(), String> {
        let mut current = self.current.lock().expect("Runtime: Mutex<CurrentPlant> envenenado");

        if let Some(running) = current.take() {
            running.control.request_reset();
            if let Err(reason) = running.wait() {
                eprintln!(
                    "[runtime] a instância anterior encerrou de forma inesperada durante reset: {reason}"
                );
            }
        }

        let running = (self.build_simulation)().spawn()?;
        let fresh_binding = running.ready.recv().map_err(|_| {
            "Runtime::reset: a nova plant thread encerrou antes de sinalizar pronta".to_string()
        })?;

        *self.binding.write().expect("Runtime: RwLock<PlantBinding> envenenado") =
            Arc::new(fresh_binding);
        *current = Some(running);
        Ok(())
    }

    /** Encerra a `Simulation` atual (mesmo mecanismo de `reset()`, sem reconstruir depois) e marca
    o `Runtime` como definitivamente parado — `wait_for_shutdown()` desbloqueia depois disso.

    Não derruba a thread do adaptador diretamente — mas, na prática, isso não sobra pendurado: a
    ÚLTIMA linha de `tep-plant/src/main.rs` é `runtime.wait_for_shutdown()`, então assim que isto
    desbloqueia, `main()` retorna e o PROCESSO INTEIRO acaba, levando junto a thread do adaptador
    (e qualquer outra) — verificado ao vivo: `control.shutdown` via OPC-UA realmente encerra o
    binário (exit code 0). O único cenário onde isso ficaria incompleto é `Runtime` embutido num
    processo maior que tem outros motivos pra continuar rodando depois de um `shutdown()` — aí sim
    a thread do adaptador vazaria, e derrubá-la de verdade exigiria passar um
    `tokio_util::CancellationToken` até `ServerBuilder::token(...)` (suportado pela própria
    `async-opcua-server`, só nunca conectado aqui) e cancelá-lo aqui. Não implementado nesta leva
    por não ser o caso de uso atual — `tep-plant` é sempre o processo inteiro.
    */
    pub fn request_shutdown(&self) {
        let mut current = self.current.lock().expect("Runtime: Mutex<CurrentPlant> envenenado");
        if let Some(running) = current.take() {
            running.control.request_reset();
            if let Err(reason) = running.wait() {
                eprintln!("[runtime] a planta encerrou de forma inesperada durante shutdown: {reason}");
            }
        }

        let (done_mutex, condvar) = &self.shutdown;
        let mut done = done_mutex.lock().expect("Runtime: Mutex<bool> de shutdown envenenado");
        *done = true;
        condvar.notify_all();
    }

    /** Bloqueia até `request_shutdown()` ser chamado — o que `tep-plant/src/main.rs` usa como sua
    única espera de verdade (o processo não tem mais nada de bloqueante pra fazer depois de subir o
    `Runtime` e, se aplicável, o adaptador).
    */
    pub fn wait_for_shutdown(&self) {
        let (done_mutex, condvar) = &self.shutdown;
        let mut done = done_mutex.lock().expect("Runtime: Mutex<bool> de shutdown envenenado");
        while !*done {
            done = condvar.wait(done).expect("Runtime: Mutex<bool> de shutdown envenenado");
        }
    }
}

/** Sobe o adaptador OPC-UA numa thread própria, pelo tempo de vida inteiro do `Runtime` — spawnado
UMA vez, nunca de novo por causa de um `reset()` (esse é exatamente o ponto: o adaptador sobrevive).
Roda num runtime tokio `current_thread` (sem pool de worker threads — não há trabalho paralelo real
a justificar um, ver `adapter/opcua.rs`).
*/
#[cfg(feature = "opcua")]
impl Runtime {
    pub fn spawn_opcua_adapter(self: &Arc<Self>, endpoint: impl Into<String>) {
        let runtime = self.clone();
        let endpoint = endpoint.into();
        std::thread::Builder::new()
            .name("adapter".to_string())
            .spawn(move || {
                let tokio_rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("adapter thread: falha ao criar runtime tokio");

                let outcome = tokio_rt.block_on(crate::adapter::opcua::serve(runtime, &endpoint));
                if let Err(err) = outcome {
                    eprintln!("[adapter] servidor OPC-UA encerrou com erro: {err}");
                }
            })
            .expect("Runtime: falha ao criar a thread do adapter");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
    use crate::sensor::model::{Ideal, Sensor as ConcreteSensor};
    use crate::state_registry::{Proxy, StateRegistry};
    use std::time::Duration;

    /* Semeia "test.frozen.value" em 42.0 e nunca mais toca nele — sem `state_keys()` (fica no
    default vazio de `DynamicModel`), então RK4 nunca integra essa chave e nada nunca chama
    `.set()` nela de novo. Só serve pra provar que um valor semeado sobrevive até virar `CurrentState`
    de verdade (depois do 1º `commit()`) e fica exatamente ali — sem a complicação de uma física que
    muda o valor a cada tick, o que tornaria qualquer asserção de igualdade sensível a quantos ticks
    já rodaram no momento exato da leitura.
    */
    struct FrozenModel {
        value: Proxy,
    }

    impl FrozenModel {
        fn new(registry: &mut StateRegistry) -> Self {
            let (offered, _) = registry.subscribe(&["test.frozen.value"], &[]);
            offered[0].set(42.0);
            ConcreteSensor::new(registry, "test.frozen.value", Box::new(Ideal));
            Self { value: offered[0].clone() }
        }
    }

    impl DynamicModel for FrozenModel {
        fn evaluate(&self) {
            let _ = &self.value; // nada a fazer — o valor não muda de propósito
        }
    }

    /* dv/dt = -v, nasce em 100.0 — mesma física de `simulation.rs::tests::DecayModel`, só que
    também publica um `Sensor` de verdade ("test.runtime.value") pra que o teste consiga ler o
    valor de FORA, através de `Runtime::binding().sensors` — o único jeito que um consumidor
    externo (um adapter, ou este teste) tem de ver o que está acontecendo dentro da Thread da
    planta, sem acesso direto ao `Proxy` (que é `!Send`, preso àquela thread).
    */
    struct DecayModel {
        value: Proxy,
        derivative: Proxy,
    }

    impl DecayModel {
        fn new(registry: &mut StateRegistry) -> Self {
            let (offered, _) = registry
                .subscribe(&["test.runtime.value", "test.runtime.value.derivative"], &[]);
            offered[0].set(100.0);
            ConcreteSensor::new(registry, "test.runtime.value", Box::new(Ideal));
            Self {
                value: offered[0].clone(),
                derivative: offered[1].clone(),
            }
        }
    }

    impl DynamicModel for DecayModel {
        fn evaluate(&self) {
            let value = self.value.get();
            self.derivative.set(-value);
        }

        fn state_keys(&self) -> Vec<String> {
            vec!["test.runtime.value".to_string()]
        }
    }

    /* `fn() -> Simulation` puro (sem captura nenhuma) — satisfaz `Fn + Send + Sync + 'static` de
    graça, exatamente o formato que `Runtime` precisa poder chamar de novo a cada `reset()`.
    */
    fn build_test_simulation() -> Simulation {
        let mut simulation = Simulation::new();
        simulation.set_tick_interval(Duration::from_millis(5));
        simulation.set_dt_hours(0.1);
        simulation.set_model(|registry, _config| DecayModel::new(registry));
        simulation
    }

    fn build_frozen_simulation() -> Simulation {
        let mut simulation = Simulation::new();
        simulation.set_tick_interval(Duration::from_millis(5));
        simulation.set_model(|registry, _config| FrozenModel::new(registry));
        simulation
    }

    #[test]
    fn new_builds_and_exposes_the_first_plant() {
        let runtime =
            Runtime::new(build_frozen_simulation).expect("Runtime::new deveria funcionar");
        let binding = runtime.binding();
        assert!(
            binding.sensors.contains_key("test.frozen.value"),
            "sensor publicado pelo FrozenModel deveria aparecer no PlantBinding inicial"
        );

        /* `ready` (que `Runtime::new()` espera) dispara logo depois do resolve() geral — ANTES do
        primeiro tick da Thread da planta ter rodado `commit()`. `Sensor::read()` reflete
        `CurrentState`, que só existe depois do primeiro commit (o valor semeado em `offered[0].set
        (42.0)` dentro de `FrozenModel::new()` só escreveu em `EvaluationState` até então) — por
        isso a pequena espera aqui, não porque `Runtime`/`PlantBinding` tenham algo assíncrono.
        `FrozenModel` não declara `state_keys()`, então o valor não muda depois disso — nenhuma
        corrida com "quantos ticks já rodaram".
        */
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(binding.sensors["test.frozen.value"].read(), 42.0);
    }

    #[test]
    fn reset_replaces_the_plant_instance_instead_of_just_pausing_it() {
        let runtime = Runtime::new(build_test_simulation).expect("Runtime::new deveria funcionar");

        // Deixa a física decair de verdade por um tempo.
        std::thread::sleep(Duration::from_millis(100));
        let decayed = runtime.binding().sensors["test.runtime.value"].read();
        assert!(
            decayed < 90.0,
            "esperava decaimento perceptível de 100.0 antes do reset, ficou em {decayed}"
        );

        runtime.reset().expect("reset() deveria funcionar");

        /* `ready` (que `reset()` espera antes de trocar o binding) dispara antes do 1º commit da
        planta NOVA — a mesma corrida do teste anterior. Uma espera mínima garante pelo menos um
        commit; como a física É decaimento, esse único tick já tira o valor de exatos 100.0 (RK4,
        dt=0.1h: ≈90.5 depois de 1 tick) — por isso o limiar abaixo (75.0) tem folga pra alguns
        ticks de jitter, mas ainda separa claramente de "continuou decaindo de onde a antiga
        parou" (que já estava < 90 ANTES do reset, e só cairia mais numa janela de espera igual).
        */
        std::thread::sleep(Duration::from_millis(10));

        /* Se `reset()` só tivesse pausado/mantido a instância antiga, o valor continuaria baixo
        (ou congelado onde estava). Uma planta genuinamente NOVA reseta pro valor semeado (100.0)
        de novo — é essa diferença que prova "substituição de instância", não "reconstrução
        cosmética" nem "a mesma planta, só re-`resolve()`'d".
        */
        let fresh = runtime.binding().sensors["test.runtime.value"].read();
        assert!(
            fresh > 75.0,
            "esperava a planta NOVA reiniciada perto de 100.0 (menos 1-2 ticks de decaimento) \
            logo após reset(), ficou em {fresh} (se ficou baixo/perto de {decayed}, o reset não \
            substituiu a instância de verdade)"
        );
    }

    #[test]
    fn binding_after_reset_routes_actuator_writes_to_the_new_plant_thread() {
        /* dynamics = |command, _state| command: a derivada É o comando, então RK4 integra a uma
        taxa constante e previsível — depois de N ticks com dt_hours=1.0, a posição avançou N*42.0
        a partir do write. Escolhido só pra este teste ser determinístico sem depender de constante
        de tempo/convergência nenhuma — o objeto sob teste é o ROTEAMENTO da escrita (o `Sender` do
        `PlantBinding` chegar na Thread da planta NOVA, não numa antiga já morta), não a física.
        */
        use crate::actuator::model::Actuator as ConcreteActuator;

        fn build_actuator_simulation() -> Simulation {
            let mut simulation = Simulation::new();
            simulation.set_tick_interval(Duration::from_millis(5));
            simulation.set_dt_hours(1.0);
            simulation.set_model(|registry, _config| {
                let actuator = ConcreteActuator::new(
                    registry,
                    "test.runtime.actuator",
                    |command, _state| command,
                );
                let mut root = Composite::new().named("test-actuator-root");
                root.add_dynamic(Box::new(actuator));
                root
            });
            simulation
        }

        let runtime =
            Runtime::new(build_actuator_simulation).expect("Runtime::new deveria funcionar");
        std::thread::sleep(Duration::from_millis(20));

        runtime.reset().expect("reset() deveria funcionar");

        let binding = runtime.binding();
        binding
            .commands
            .send(("test.runtime.actuator".to_string(), 42.0))
            .expect("o Sender do PlantBinding pós-reset deveria ter um receptor vivo do outro lado");

        // Espera tempo suficiente pra pelo menos alguns ticks rodarem com o comando já aplicado.
        std::thread::sleep(Duration::from_millis(30));

        let shadow = binding
            .actuators
            .get("test.runtime.actuator")
            .expect("atuador deveria estar catalogado no PlantBinding pós-reset");
        assert!(
            shadow.read() > 10.0,
            "escrita enviada DEPOIS do reset deveria ter sido drenada e integrada pela Thread da \
            planta NOVA (posição ficou em {}); se ficou em 0.0, o comando foi mandado pra um \
            Sender cujo receptor já não existe mais (a Thread da planta antiga)",
            shadow.read(),
        );
    }
}
