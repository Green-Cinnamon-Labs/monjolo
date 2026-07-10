// sensor/model.rs
//
// Sensor (ver docs/issue55_opcua_refactor/plan_refactor.md, seções 3.5-3.9):
// não participa de evaluate()/EvaluationState, não é DynamicModel. Lê
// CurrentState via um ReadProxy resolvido uma vez na construção — nunca faz
// lookup por string no caminho quente de leitura.
//
// Agnóstico ao que o sinal significa (vazão, pressão, temperatura...) — isso
// é metadado de quem declara o sensor (tag/unidade), não parte do tipo.
// Acompanha exatamente uma variável (uma chave); quem quer observar A, B e C
// declara três sensores.

use std::cell::RefCell;
use std::rc::Rc;

use rand::rngs::SmallRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Normal};

use crate::state_registry::{ReadProxy, StateRegistry};

/** O que acontece entre o valor bruto lido do registry e o valor devolvido
pelo sensor. Pode ter estado interno (ex.: última leitura, para
histerese/ruído) sem que isso implique dinâmica integrada — esse estado
não entra no vetor que o `Integrator` avança, só é atualizado como efeito
colateral de cada `read()`.
*/
pub trait SensorBehavior {
    fn apply(&mut self, physical_value: f64) -> f64;
}

/** Sensor: acompanha uma única chave do `StateRegistry`, sempre em
`CurrentState` — nunca em `EvaluationState`, nunca um valor hipotético de
sub-passo do integrador. Um pipe de leitura: lê o valor bruto confirmado via
`ReadProxy` e aplica um `SensorBehavior` (ideal, ruído, histerese, ...) antes
de expor.

A chave é resolvida **uma única vez**, na construção, contra um `ReadProxy` —
depois disso não há mais lookup por string: `read()` só indexa direto no
buffer de `CurrentState`. Por isso `Sensor::new()` só deve ser chamado depois
que todo `DynamicModel` já se inscreveu (`subscribe()`) e
`StateRegistry::resolve()` geral já rodou (seção 3.8 do plano) — antes disso
a chave pode não existir ainda.
*/
pub struct Sensor {
    proxy: ReadProxy,
    behavior: Box<dyn SensorBehavior>,
}

impl Sensor {
    /// Erra se `key` ainda não existir em `CurrentState` — sinal de que
    /// `Sensor::new()` foi chamado cedo demais (antes do `resolve()` geral)
    /// ou de que nenhum componente oferece esse nome.
    pub fn new(
        registry: Rc<RefCell<StateRegistry>>,
        key: &str,
        behavior: Box<dyn SensorBehavior>,
    ) -> Result<Self, String> {
        let proxy = registry.borrow().read_proxy(key).ok_or_else(|| format!(
            "Sensor: chave '{key}' não existe em CurrentState — StateRegistry::resolve() já rodou e nenhum componente oferece esse slot?"
        ))?;
        Ok(Self { proxy, behavior })
    }

    /// Lê o valor confirmado via `ReadProxy` (nunca hipotético) e aplica o
    /// comportamento do sensor. Sem lookup por string — só indexação direta.
    pub fn read(&mut self) -> f64 {
        self.behavior.apply(self.proxy.get())
    }
}

// ── Ideal — sem transformação ─────────────────────────────────────────────────

pub struct Ideal;

impl SensorBehavior for Ideal {
    fn apply(&mut self, physical_value: f64) -> f64 {
        physical_value
    }
}

// ── Noisy — ruído gaussiano ────────────────────────────────────────────────────

pub struct Noisy {
    std_dev: f64,
    rng: SmallRng,
}

impl Noisy {
    pub fn new(std_dev: f64, seed: u64) -> Self {
        Self {
            std_dev,
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl SensorBehavior for Noisy {
    fn apply(&mut self, physical_value: f64) -> f64 {
        if self.std_dev == 0.0 {
            return physical_value;
        }
        let dist = Normal::new(0.0, self.std_dev).expect("invalid std_dev");
        physical_value + dist.sample(&mut self.rng)
    }
}

// ── Hysteresis — banda morta em torno da última leitura ────────────────────────

pub struct Hysteresis {
    deadband: f64,
    last_output: Option<f64>,
}

impl Hysteresis {
    pub fn new(deadband: f64) -> Self {
        Self {
            deadband,
            last_output: None,
        }
    }
}

impl SensorBehavior for Hysteresis {
    fn apply(&mut self, physical_value: f64) -> f64 {
        let output = match self.last_output {
            Some(prev) if (physical_value - prev).abs() < self.deadband => prev,
            _ => physical_value,
        };
        self.last_output = Some(output);
        output
    }
}
