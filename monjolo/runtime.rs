/* monjolo/runtime.rs */

/* Documentação em `docs/runtime.md`; testes em `tests/runtime.rs`. */
use std::sync::{Arc, Condvar, Mutex, RwLock};

use crate::numerical_method::NumericalMethod;
use crate::simulation::{PlantBinding, RunningSimulation, Simulation};

const CONFIG_PATH: &str = "application.toml";
const EXECUTION_ORDER_PATH: &str = "execution_order.txt";

#[cfg(feature = "opcua")]
const DEFAULT_OPCUA_ENDPOINT: &str = "opc.tcp://127.0.0.1:4840/tep/server/";
#[cfg(feature = "function")]
const DEFAULT_FUNCTION_ENDPOINT: &str = "127.0.0.1:8091";

const MODE_ENV: &str = "MONJOLO_MODE";
#[cfg(feature = "function")]
const FUNCTION_ENDPOINT_ENV: &str = "MONJOLO_FUNCTION_ENDPOINT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Simulation,
    Function,
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "simulation" => Ok(Mode::Simulation),
            "function" => Ok(Mode::Function),
            other => Err(format!("modo desconhecido: '{other}' (esperava \"simulation\" ou \"function\")")),
        }
    }
}

#[derive(Debug)]
pub struct BootstrapSettings {
    pub mode: Mode,
    pub numerical_method: NumericalMethod,
    #[cfg(feature = "opcua")]
    pub opcua_endpoint: String,
    #[cfg(feature = "function")]
    pub function_endpoint: String,
}

impl BootstrapSettings {
    fn load(path: &str) -> Result<Self, String> {
        let mode_override = std::env::var(MODE_ENV).ok();
        #[cfg(feature = "function")]
        let endpoint_override = std::env::var(FUNCTION_ENDPOINT_ENV).ok();
        #[cfg(not(feature = "function"))]
        let endpoint_override: Option<String> = None;
        Self::parse(path, mode_override.as_deref(), endpoint_override.as_deref())
    }

    pub fn parse(
        path: &str,
        mode_override: Option<&str>,
        function_endpoint_override: Option<&str>,
    ) -> Result<Self, String> {
        let root: Option<toml::Value> = match std::fs::read_to_string(path) {
            Ok(content) => Some(
                toml::from_str(&content)
                    .map_err(|e| format!("bootstrap: erro parseando TOML '{path}': {e}"))?,
            ),
            Err(_) => None,
        };

        let monjolo_table = root.as_ref().and_then(|r| r.get("monjolo"));

        let mode = match mode_override.or_else(|| monjolo_table.and_then(|m| m.get("mode")).and_then(|v| v.as_str())) {
            Some(raw) => raw.parse().map_err(|e| format!("bootstrap: {e}"))?,
            None => Mode::default(),
        };

        let numerical_method = match monjolo_table
            .and_then(|m| m.get("numerical_method"))
            .and_then(|v| v.as_str())
        {
            Some(raw) => raw.parse().map_err(|e| format!("bootstrap: {e}"))?,
            None => NumericalMethod::default(),
        };

        #[cfg(feature = "opcua")]
        let opcua_endpoint = monjolo_table
            .and_then(|m| m.get("opcua"))
            .and_then(|o| o.get("endpoint"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_OPCUA_ENDPOINT)
            .to_string();

        #[cfg(feature = "function")]
        let function_endpoint = function_endpoint_override
            .or_else(|| {
                monjolo_table
                    .and_then(|m| m.get("function"))
                    .and_then(|f| f.get("endpoint"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or(DEFAULT_FUNCTION_ENDPOINT)
            .to_string();
        #[cfg(not(feature = "function"))]
        let _ = function_endpoint_override;

        Ok(Self {
            mode,
            numerical_method,
            #[cfg(feature = "opcua")]
            opcua_endpoint,
            #[cfg(feature = "function")]
            function_endpoint,
        })
    }
}

type SimulationFactory = dyn Fn() -> Simulation + Send + Sync;

pub struct Runtime {
    build_simulation: Box<SimulationFactory>,
    binding: RwLock<Arc<PlantBinding>>,
    /* `None` só dentro de `reset()`/`request_shutdown()`, com o mutex trancado. */
    current: Mutex<Option<RunningSimulation>>,
    shutdown: (Mutex<bool>, Condvar),
}

impl Runtime {
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

    pub fn binding(&self) -> Arc<PlantBinding> {
        self.binding
            .read()
            .expect("Runtime: RwLock<PlantBinding> envenenado")
            .clone()
    }

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

    pub fn wait_for_shutdown(&self) {
        let (done_mutex, condvar) = &self.shutdown;
        let mut done = done_mutex.lock().expect("Runtime: Mutex<bool> de shutdown envenenado");
        while !*done {
            done = condvar.wait(done).expect("Runtime: Mutex<bool> de shutdown envenenado");
        }
    }

    pub fn bootstrap() -> Result<Application, String> {
        if let Err(err) = std::fs::write(EXECUTION_ORDER_PATH, crate::component::describe_phase_a_execution_order()) {
            eprintln!("[bootstrap] não consegui escrever {EXECUTION_ORDER_PATH}: {err}");
        }

        let settings = BootstrapSettings::load(CONFIG_PATH)?;

        if settings.mode == Mode::Function {
            #[cfg(feature = "function")]
            {
                let config = crate::snapshot::Snapshot::from_file(CONFIG_PATH)?;
                let function = crate::model_function::ModelFunction::discovered(&config)?;
                let server = crate::adapter::function::FunctionServer::bind(&settings.function_endpoint)?;
                eprintln!(
                    "[function] servindo {} chave(s), {} estado(s), em http://{}",
                    function.keys().len(),
                    function.state_keys().len(),
                    server.local_address(),
                );
                return Ok(Application::Function { server, function });
            }
            #[cfg(not(feature = "function"))]
            return Err(
                "bootstrap: modo \"function\" pedido, mas este binário foi compilado sem a feature `function`"
                    .to_string(),
            );
        }

        let numerical_method = settings.numerical_method;

        let runtime = Runtime::new(move || {
            let mut simulation = Simulation::new();
            simulation.set_config_path(CONFIG_PATH);
            simulation.set_numerical_method(numerical_method);
            simulation
        })?;

        #[cfg(feature = "opcua")]
        runtime.spawn_opcua_adapter(settings.opcua_endpoint);

        Ok(Application::Simulation(runtime))
    }

    pub fn run(&self) {
        #[cfg(feature = "opcua")]
        self.wait_for_shutdown();
        #[cfg(not(feature = "opcua"))]
        loop {
            std::thread::park();
        }
    }
}

pub enum Application {
    Simulation(Arc<Runtime>),
    #[cfg(feature = "function")]
    Function {
        server: crate::adapter::function::FunctionServer,
        function: crate::model_function::ModelFunction,
    },
}

impl Application {
    pub fn run(&self) {
        match self {
            Application::Simulation(runtime) => runtime.run(),
            #[cfg(feature = "function")]
            Application::Function { server, function } => server.serve(function),
        }
    }
}

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
