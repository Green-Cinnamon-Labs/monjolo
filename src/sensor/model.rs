// sensor/model.rs
//
// Sensor (ver docs/issue55_opcua_refactor/plan_refactor.md, seções 3.5-3.9;
// plan_refactor_legislativo.md, Art. 3.6/11.9): não participa de
// evaluate()/EvaluationState, não é DynamicModel. Lê CurrentState via um
// ReadProxy resolvido uma vez na construção — nunca faz lookup por string
// no caminho quente de leitura.
//
// Agnóstico ao que o sinal significa (vazão, pressão, temperatura...) — isso
// é metadado de quem declara o sensor (tag/unidade), não parte do tipo.
// Acompanha exatamente uma variável (uma chave); quem quer observar A, B e C
// declara três sensores.
//
// Send + Sync: Sensor é compartilhável via Arc<Sensor> entre a Thread da
// planta, a Thread do Adaptador e um futuro Controlador — nenhum deles tem
// cópia própria, todos apontam pro mesmo instrumento. `read()` é `&self`
// (não `&mut self`): a mutação de SensorBehavior fica atrás de um Mutex
// interno, protegida contra chamadas concorrentes de threads diferentes.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use rand::rngs::SmallRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Normal};

use crate::state_registry::{ReadProxy, StateRegistry};

/** O que acontece entre o valor bruto lido do registry e o valor devolvido
pelo sensor. Pode ter estado interno (ex.: última leitura, para
histerese/ruído) sem que isso implique dinâmica integrada — esse estado
não entra no vetor que o `Integrator` avança, só é atualizado como efeito
colateral de cada `read()`. `Send` porque vive dentro de `Sensor`
(compartilhado entre threads via `Arc<Sensor>`, Art. 3.6.6 do plano
legislativo) atrás de um `Mutex` — nunca `Sync` diretamente, ninguém chama
`apply()` sem passar pelo lock.
*/
pub trait SensorBehavior: Send {
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

Camada de medição sobre o estado físico confirmado (Art. 1.3 §1º, 3.6.6 do
plano legislativo) — nunca expõe `current_state` bruto pra quem consome via
`Sensor`: todo consumidor (cliente OPC-UA, futuro Controlador) só enxerga o
valor já passado por `SensorBehavior`.
*/
pub struct Sensor {
    proxy: ReadProxy,
    inner: Mutex<SensorInner>,
}

struct SensorInner {
    behavior: Box<dyn SensorBehavior>,
    /** `(generation do CurrentState em que este valor foi calculado, valor
    já processado)` — cache de idempotência (Art. 3.6.6): garante que
    `SensorBehavior::apply()` só avança (amostra ruído, reavalia histerese)
    uma vez por `commit()`, não uma vez por chamada de `read()`. Duas
    leituras do mesmo sensor, de threads diferentes, dentro da mesma
    `generation`, sempre devolvem o mesmo valor.
    */
    cached: Option<(u64, f64)>,
}

impl Sensor {
    /** Erra se `key` ainda não existir em `CurrentState` — sinal de que
    `Sensor::new()` foi chamado cedo demais (antes do `resolve()` geral) ou
    de que nenhum componente oferece esse nome.
    */
    pub fn new(
        registry: Rc<RefCell<StateRegistry>>,
        key: &str,
        behavior: Box<dyn SensorBehavior>,
    ) -> Result<Self, String> {
        let proxy = registry.borrow().read_proxy(key).ok_or_else(|| format!(
            "Sensor: chave '{key}' não existe em CurrentState — StateRegistry::resolve() já rodou e nenhum componente oferece esse slot?"
        ))?;
        Ok(Self {
            proxy,
            inner: Mutex::new(SensorInner {
                behavior,
                cached: None,
            }),
        })
    }

    /** Lê o valor confirmado (nunca hipotético) e aplica o `SensorBehavior`
    — idempotente dentro da mesma `generation` de `CurrentState` (Art. 3.6.6
    do plano legislativo): a primeira chamada depois de um `commit()`
    invoca `SensorBehavior::apply()` de verdade e guarda o resultado;
    qualquer chamada seguinte — de qualquer consumidor, de qualquer thread
    — antes do próximo `commit()`, só devolve o valor já cacheado, sem
    reamostrar ruído nem reavaliar histerese duas vezes pro mesmo instante
    confirmado.
    */
    pub fn read(&self) -> f64 {
        let (generation, raw) = self.proxy.get_versioned();
        let mut inner = self.inner.lock().expect("Sensor: lock interno envenenado");
        if let Some((cached_generation, value)) = inner.cached {
            if cached_generation == generation {
                return value;
            }
        }
        let value = inner.behavior.apply(raw);
        inner.cached = Some((generation, value));
        value
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /** Behavior de teste que conta quantas vezes `apply()` foi de fato
    invocado — é essa contagem que prova a idempotência (Art. 3.6.6): deve
    subir uma vez por `commit()`, nunca uma vez por `read()`.
    */
    struct CountingBehavior {
        calls: Arc<AtomicUsize>,
    }

    impl SensorBehavior for CountingBehavior {
        fn apply(&mut self, physical_value: f64) -> f64 {
            self.calls.fetch_add(1, Ordering::SeqCst);
            physical_value
        }
    }

    #[test]
    fn read_is_idempotent_within_the_same_generation() {
        let registry = StateRegistry::shared();
        let (offered, _) = registry.borrow_mut().subscribe(&["reactor.temperature"], &[]);
        offered[0].set(120.5);
        registry.borrow_mut().resolve().unwrap();
        registry.borrow_mut().commit();

        let calls = Arc::new(AtomicUsize::new(0));
        let sensor = Sensor::new(
            registry.clone(),
            "reactor.temperature",
            Box::new(CountingBehavior {
                calls: calls.clone(),
            }),
        )
        .unwrap();

        // Múltiplas leituras, mesma generation (nenhum commit() no meio):
        // behavior só roda na primeira.
        assert_eq!(sensor.read(), 120.5);
        assert_eq!(sensor.read(), 120.5);
        assert_eq!(sensor.read(), 120.5);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "apply() deveria rodar uma única vez dentro da mesma generation"
        );

        // Novo valor + novo commit() = nova generation: agora sim o
        // behavior roda de novo.
        offered[0].set(121.0);
        registry.borrow_mut().commit();

        assert_eq!(sensor.read(), 121.0);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "apply() deveria rodar de novo depois de um novo commit()"
        );
    }

    #[test]
    fn sensor_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Sensor>();
    }

    /** Cenário real que motivou a mudança: um cliente OPC-UA e um futuro
    Controlador, em threads de verdade, lendo o mesmo sensor "ao mesmo
    tempo" (mesma generation). Todos devem ver o mesmo valor, e o behavior
    só pode ter rodado uma vez.
    */
    #[test]
    fn concurrent_reads_from_real_threads_are_idempotent() {
        let registry = StateRegistry::shared();
        let (offered, _) = registry.borrow_mut().subscribe(&["reactor.temperature"], &[]);
        offered[0].set(87.5);
        registry.borrow_mut().resolve().unwrap();
        registry.borrow_mut().commit();

        let calls = Arc::new(AtomicUsize::new(0));
        let sensor = Arc::new(
            Sensor::new(
                registry.clone(),
                "reactor.temperature",
                Box::new(CountingBehavior {
                    calls: calls.clone(),
                }),
            )
            .unwrap(),
        );

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let sensor = sensor.clone();
                std::thread::spawn(move || sensor.read())
            })
            .collect();

        let values: Vec<f64> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        assert!(
            values.iter().all(|&v| v == 87.5),
            "todas as leituras concorrentes deveriam ver o mesmo valor: {values:?}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "apply() deveria rodar uma única vez mesmo com 8 threads lendo concorrentemente"
        );
    }
}
