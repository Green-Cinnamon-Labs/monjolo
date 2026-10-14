/* tests/runtime.rs

Prova `monjolo::runtime` (issues #67 e #68, spec-tennessee-eastman#94) só pela interface pública: a
leitura de `[monjolo]` em `application.toml` (`BootstrapSettings::parse`) e o comportamento do
`Runtime` com modelos de brinquedo. O que o módulo garante está descrito em `docs/runtime.md`.
*/

use std::time::Duration;

use monjolo::actuator::model::Actuator as ConcreteActuator;
use monjolo::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
use monjolo::numerical_method::NumericalMethod;
use monjolo::runtime::{BootstrapSettings, Mode, Runtime};
use monjolo::sensor::model::{Ideal, Sensor as ConcreteSensor};
use monjolo::simulation::Simulation;
use monjolo::state_registry::{Proxy, StateRegistry};

/* Escreve `content` num arquivo temporário com nome único (evita colisão entre testes rodando em
paralelo, já que `cargo test` não serializa por padrão) e sempre limpa o arquivo depois — mesmo se
o teste panicar (`Drop`, não um `finally` manual).
*/
struct TempTomlFile {
    path: std::path::PathBuf,
}

impl TempTomlFile {
    fn new(name: &str, content: &str) -> Self {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, content).expect("falha ao escrever TOML temporário de teste");
        Self { path }
    }

    fn path_str(&self) -> &str {
        self.path.to_str().expect("caminho temporário deveria ser UTF-8 válido")
    }
}

impl Drop for TempTomlFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/* Lê a configuração como `bootstrap()` leria num ambiente sem nenhuma das variáveis definidas. */
fn parse(path: &str) -> Result<BootstrapSettings, String> {
    BootstrapSettings::parse(path, None, None)
}

#[test]
fn bootstrap_settings_default_when_file_is_absent() {
    let path = std::env::temp_dir().join("monjolo_bootstrap_test_does_not_exist.toml");
    let _ = std::fs::remove_file(&path);
    let settings = parse(path.to_str().unwrap()).expect("arquivo ausente não deveria ser erro fatal");
    assert!(matches!(settings.numerical_method, NumericalMethod::RK4));
    assert_eq!(settings.mode, Mode::Simulation);
}

#[test]
fn bootstrap_settings_reads_numerical_method_from_monjolo_table() {
    let file = TempTomlFile::new(
        "monjolo_bootstrap_test_numerical_method.toml",
        "[monjolo]\nnumerical_method = \"RK4\"\n",
    );
    let settings = parse(file.path_str()).expect("TOML válido não deveria falhar");
    assert!(matches!(settings.numerical_method, NumericalMethod::RK4));
}

#[test]
fn bootstrap_settings_mode_defaults_to_simulation_and_reads_function_from_monjolo_table() {
    let absent = TempTomlFile::new(
        "monjolo_bootstrap_test_mode_absent.toml",
        "[monjolo]\nnumerical_method = \"rk4\"\n",
    );
    let settings = parse(absent.path_str()).expect("TOML válido");
    assert_eq!(settings.mode, Mode::Simulation);

    let function = TempTomlFile::new("monjolo_bootstrap_test_mode_function.toml", "[monjolo]\nmode = \"function\"\n");
    let settings = parse(function.path_str()).expect("TOML válido");
    assert_eq!(settings.mode, Mode::Function);
}

/* O que veio do ambiente (`MONJOLO_MODE`) ganha do arquivo — nos dois sentidos. */
#[test]
fn bootstrap_settings_mode_override_wins_over_the_file() {
    let file = TempTomlFile::new("monjolo_bootstrap_test_mode_override.toml", "[monjolo]\nmode = \"simulation\"\n");

    let settings = BootstrapSettings::parse(file.path_str(), Some("function"), None).expect("TOML válido");
    assert_eq!(settings.mode, Mode::Function);

    let file = TempTomlFile::new("monjolo_bootstrap_test_mode_override_back.toml", "[monjolo]\nmode = \"function\"\n");
    let settings = BootstrapSettings::parse(file.path_str(), Some("Simulation"), None).expect("TOML válido");
    assert_eq!(settings.mode, Mode::Simulation);
}

#[test]
fn bootstrap_settings_errors_on_unknown_mode() {
    let file = TempTomlFile::new("monjolo_bootstrap_test_bad_mode.toml", "[monjolo]\nmode = \"batch\"\n");
    let err = parse(file.path_str())
        .expect_err("modo desconhecido deveria ser erro fatal, não um default silencioso");
    assert!(err.contains("batch"), "mensagem de erro deveria citar o valor inválido: {err}");
}

#[cfg(feature = "function")]
#[test]
fn bootstrap_settings_function_endpoint_comes_from_override_then_file_then_default() {
    let file = TempTomlFile::new(
        "monjolo_bootstrap_test_function_endpoint.toml",
        "[monjolo.function]\nendpoint = \"0.0.0.0:9000\"\n",
    );
    let settings = BootstrapSettings::parse(file.path_str(), None, Some("127.0.0.1:7000")).expect("TOML válido");
    assert_eq!(settings.function_endpoint, "127.0.0.1:7000");

    let settings = parse(file.path_str()).expect("TOML válido");
    assert_eq!(settings.function_endpoint, "0.0.0.0:9000");

    let absent = TempTomlFile::new("monjolo_bootstrap_test_function_endpoint_default.toml", "[monjolo]\n");
    let settings = parse(absent.path_str()).expect("TOML válido");
    assert_eq!(settings.function_endpoint, "127.0.0.1:8091");
}

#[test]
fn bootstrap_settings_errors_on_unknown_numerical_method() {
    let file = TempTomlFile::new(
        "monjolo_bootstrap_test_bad_numerical_method.toml",
        "[monjolo]\nnumerical_method = \"euler\"\n",
    );
    let err = parse(file.path_str())
        .expect_err("método numérico desconhecido deveria ser erro fatal, não um default silencioso");
    assert!(err.contains("euler"), "mensagem de erro deveria citar o valor inválido: {err}");
}

/* Mesmo arquivo real que `Simulation`/`Snapshot` consomem pras chaves físicas (`[state.*]`) e
`[meta]` — prova que ler `[monjolo]` não colide com, nem exige nada de, o resto do arquivo que
`application.toml` de verdade sempre teve.
*/
#[test]
fn bootstrap_settings_ignores_unrelated_tables_like_state_and_meta() {
    let file = TempTomlFile::new(
        "monjolo_bootstrap_test_mixed_file.toml",
        "[meta]\ndescription = \"não é numérico, Snapshot ignora\"\n\n\
         [state.reactor]\nenergy = 3.25\n\n\
         [monjolo]\nnumerical_method = \"rk4\"\n",
    );
    let settings = parse(file.path_str())
        .expect("tabelas [meta]/[state.*] não deveriam atrapalhar a leitura de [monjolo]");
    assert!(matches!(settings.numerical_method, NumericalMethod::RK4));
}

#[cfg(feature = "opcua")]
#[test]
fn bootstrap_settings_reads_opcua_endpoint_override_and_falls_back_to_default() {
    let overridden = TempTomlFile::new(
        "monjolo_bootstrap_test_opcua_endpoint.toml",
        "[monjolo.opcua]\nendpoint = \"opc.tcp://0.0.0.0:9999/custom/\"\n",
    );
    let settings = parse(overridden.path_str()).expect("TOML válido não deveria falhar");
    assert_eq!(settings.opcua_endpoint, "opc.tcp://0.0.0.0:9999/custom/");

    let absent_table = TempTomlFile::new(
        "monjolo_bootstrap_test_opcua_endpoint_default.toml",
        "[monjolo]\nnumerical_method = \"rk4\"\n",
    );
    let settings = parse(absent_table.path_str()).expect("TOML válido não deveria falhar");
    assert_eq!(settings.opcua_endpoint, "opc.tcp://127.0.0.1:4840/tep/server/");
}

/* Semeia "test.frozen.value" em 42.0 e nunca mais toca nele — sem `state_keys()` (fica no default
vazio de `DynamicModel`), então RK4 nunca integra essa chave e nada nunca chama `.set()` nela de
novo. Só serve pra provar que um valor semeado sobrevive até virar `CurrentState` de verdade (depois
do 1º `commit()`) e fica exatamente ali — sem a complicação de uma física que muda o valor a cada
tick, o que tornaria qualquer asserção de igualdade sensível a quantos ticks já rodaram no momento
exato da leitura.
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
        let _ = &self.value;
    }
}

/* dv/dt = -v, nasce em 100.0, e publica um `Sensor` de verdade ("test.runtime.value") pra que o
teste consiga ler o valor de FORA, através de `Runtime::binding().sensors` — o único jeito que um
consumidor externo (um adapter, ou este teste) tem de ver o que está acontecendo dentro da Thread da
planta, sem acesso direto ao `Proxy` (que é `!Send`, preso àquela thread).
*/
struct DecayModel {
    value: Proxy,
    derivative: Proxy,
}

impl DecayModel {
    fn new(registry: &mut StateRegistry) -> Self {
        let (offered, _) =
            registry.subscribe(&["test.runtime.value", "test.runtime.value.derivative"], &[]);
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

/* `fn() -> Simulation` puro (sem captura nenhuma) — satisfaz `Fn + Send + Sync + 'static` de graça,
exatamente o formato que `Runtime` precisa poder chamar de novo a cada `reset()`.
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
    let runtime = Runtime::new(build_frozen_simulation).expect("Runtime::new deveria funcionar");
    let binding = runtime.binding();
    assert!(
        binding.sensors.contains_key("test.frozen.value"),
        "sensor publicado pelo FrozenModel deveria aparecer no PlantBinding inicial"
    );

    /* `ready` (que `Runtime::new()` espera) dispara logo depois do resolve() geral — ANTES do
    primeiro tick da Thread da planta ter rodado `commit()`. `Sensor::read()` reflete
    `CurrentState`, que só existe depois do primeiro commit (o valor semeado em
    `FrozenModel::new()` só escreveu em `EvaluationState` até então) — por isso a pequena espera
    aqui, não porque `Runtime`/`PlantBinding` tenham algo assíncrono. `FrozenModel` não declara
    `state_keys()`, então o valor não muda depois disso — nenhuma corrida com "quantos ticks já
    rodaram".
    */
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(binding.sensors["test.frozen.value"].read(), 42.0);
}

#[test]
fn reset_replaces_the_plant_instance_instead_of_just_pausing_it() {
    let runtime = Runtime::new(build_test_simulation).expect("Runtime::new deveria funcionar");

    /* Deixa a física decair de verdade por um tempo. */
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
    dt=0.1h: ≈90.5 depois de 1 tick) — por isso o limiar abaixo (75.0) tem folga pra alguns ticks
    de jitter, mas ainda separa claramente de "continuou decaindo de onde a antiga parou" (que já
    estava < 90 ANTES do reset, e só cairia mais numa janela de espera igual).
    */
    std::thread::sleep(Duration::from_millis(10));

    /* Se `reset()` só tivesse pausado/mantido a instância antiga, o valor continuaria baixo (ou
    congelado onde estava). Uma planta genuinamente NOVA reseta pro valor semeado (100.0) de novo
    — é essa diferença que prova "substituição de instância", não "reconstrução cosmética" nem "a
    mesma planta, só re-`resolve()`'d".
    */
    let fresh = runtime.binding().sensors["test.runtime.value"].read();
    assert!(
        fresh > 75.0,
        "esperava a planta NOVA reiniciada perto de 100.0 (menos 1-2 ticks de decaimento) logo \
        após reset(), ficou em {fresh} (se ficou baixo/perto de {decayed}, o reset não substituiu \
        a instância de verdade)"
    );
}

/* dynamics = |command, _state| command: a derivada É o comando, então RK4 integra a uma taxa
constante e previsível — depois de N ticks com dt_hours=1.0, a posição avançou N*42.0 a partir do
write. Escolhido só pra este teste ser determinístico sem depender de constante de tempo/
convergência nenhuma — o objeto sob teste é o ROTEAMENTO da escrita (o `Sender` do `PlantBinding`
chegar na Thread da planta NOVA, não numa antiga já morta), não a física.
*/
fn build_actuator_simulation() -> Simulation {
    let mut simulation = Simulation::new();
    simulation.set_tick_interval(Duration::from_millis(5));
    simulation.set_dt_hours(1.0);
    simulation.set_model(|registry, _config| {
        let actuator = ConcreteActuator::new(registry, "test.runtime.actuator", |command, _state| command);
        let mut root = Composite::new().named("test-actuator-root");
        root.add_dynamic(Box::new(actuator));
        root
    });
    simulation
}

#[test]
fn binding_after_reset_routes_actuator_writes_to_the_new_plant_thread() {
    let runtime = Runtime::new(build_actuator_simulation).expect("Runtime::new deveria funcionar");
    std::thread::sleep(Duration::from_millis(20));

    runtime.reset().expect("reset() deveria funcionar");

    let binding = runtime.binding();
    binding
        .commands
        .send(("test.runtime.actuator".to_string(), 42.0))
        .expect("o Sender do PlantBinding pós-reset deveria ter um receptor vivo do outro lado");

    /* Espera tempo suficiente pra pelo menos alguns ticks rodarem com o comando já aplicado. */
    std::thread::sleep(Duration::from_millis(30));

    let shadow = binding
        .actuators
        .get("test.runtime.actuator")
        .expect("atuador deveria estar catalogado no PlantBinding pós-reset");
    assert!(
        shadow.read() > 10.0,
        "escrita enviada DEPOIS do reset deveria ter sido drenada e integrada pela Thread da \
        planta NOVA (posição ficou em {}); se ficou em 0.0, o comando foi mandado pra um Sender \
        cujo receptor já não existe mais (a Thread da planta antiga)",
        shadow.read(),
    );
}
