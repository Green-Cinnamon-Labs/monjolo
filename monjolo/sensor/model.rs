/** sensor/model.rs

Implementação concreta de `Sensor` (o trait de `sensor/mod.rs`) — bloco genérico e reaproveitável do
framework: ruído, histerese e cache de idempotência não têm nada de específico do TEP, qualquer
planta montada sobre `monjolo` pode usar isso.

`Sensor` não participa de evaluate()/EvaluationState, não é DynamicModel. Lê CurrentState via um
ReadProxy declarado na construção (`StateRegistry::subscribe_read()`) e resolvido depois, junto com
tudo mais, por `StateRegistry::resolve()` — nunca faz lookup por string no caminho quente de
leitura. Agnóstico ao que o sinal significa (vazão, pressão, temperatura...) — isso é metadado de
quem declara o sensor, não parte do tipo.
*/

use std::sync::{Arc, Mutex};

use rand::rngs::SmallRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Normal};

use crate::state_registry::{ReadProxy, StateRegistry};

/** O que acontece entre o valor bruto lido do registry e o valor devolvido pelo sensor. Pode ter
estado interno (ex.: última leitura, para histerese/ruído) sem que isso implique dinâmica integrada
— esse estado não entra no vetor que o `Integrator` avança, só é atualizado como efeito colateral de
cada leitura.
*/
pub trait SensorBehavior: Send {
    fn apply(&mut self, physical_value: f64) -> f64;
}

/** Sensor: acompanha uma única chave do `StateRegistry`, sempre em `CurrentState` — nunca em
`EvaluationState`, nunca um valor hipotético de sub-passo do integrador. Um pipe de leitura: lê o
valor bruto confirmado via `ReadProxy` e aplica um `SensorBehavior` (ideal, ruído, histerese, ...)
antes de expor. `Send + Sync`: compartilhável via `Arc<Sensor>` entre threads.
*/
pub struct Sensor {
    proxy: ReadProxy,
    inner: Mutex<SensorInner>,
}

struct SensorInner {
    behavior: Box<dyn SensorBehavior>,
    /** `(generation do CurrentState em que este valor foi calculado, valor já processado)` — cache
    de idempotência: garante que `SensorBehavior::apply()` só avança (amostra ruído, reavalia
    histerese) uma vez por `commit()`, não uma vez por chamada de leitura.
    */
    cached: Option<(u64, f64)>,
}

impl Sensor {
    /** Declara a necessidade da chave `key` via `StateRegistry::subscribe_read()` — mesmo ciclo
    declare → register → resolve → inject de qualquer outro `need` (Art. 6.3/7.1): não erra mais na
    hora, só quando `StateRegistry::resolve()` rodar e não encontrar provedor pra `key`. Pode ser
    chamado em qualquer ordem em relação a quem oferece essa chave — inclusive antes.

    Devolve `Arc<Self>`, não `Self`: "criado = já oferecido" é invariante do tipo, não etapa manual
    de quem constrói — `new()` já registra o sensor no catálogo de `StateRegistry` sob a própria
    `key` (`offer_sensor()`, `pub(crate)`) antes de devolver, e devolve o mesmo `Arc` que guardou
    lá — não uma cópia.
    */
    pub fn new(registry: &mut StateRegistry, key: &str, behavior: Box<dyn SensorBehavior>) -> Arc<Self> {
        let proxy = registry.subscribe_read(&[key]).remove(0);
        let sensor = Arc::new(Self {
            proxy,
            inner: Mutex::new(SensorInner {
                behavior,
                cached: None,
            }),
        });
        registry.offer_sensor(key, sensor.clone());
        sensor
    }
}

impl super::Sensor for Sensor {
    /** Lê o valor confirmado (nunca hipotético) e aplica o `SensorBehavior` — idempotente dentro da
    mesma `generation` de `CurrentState`: a primeira chamada depois de um `commit()` invoca
    `SensorBehavior::apply()` de verdade e guarda o resultado; qualquer chamada seguinte — de
    qualquer consumidor, de qualquer thread — antes do próximo `commit()`, só devolve o valor já
    cacheado.
    */
    fn read(&self) -> f64 {
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

/* ── Ideal — sem transformação ── */

pub struct Ideal;

impl SensorBehavior for Ideal {
    fn apply(&mut self, physical_value: f64) -> f64 {
        physical_value
    }
}

/* ── Noisy — ruído gaussiano ── */

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

/* ── Hysteresis — banda morta em torno da última leitura ── */

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
    use crate::sensor::Sensor as SensorTrait;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

        let calls = Arc::new(AtomicUsize::new(0));
        let sensor = Sensor::new(
            &mut registry.borrow_mut(),
            "reactor.temperature",
            Box::new(CountingBehavior {
                calls: calls.clone(),
            }),
        );

        registry.borrow_mut().resolve().unwrap();
        registry.borrow_mut().commit();

        assert_eq!(sensor.read(), 120.5);
        assert_eq!(sensor.read(), 120.5);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        offered[0].set(121.0);
        registry.borrow_mut().commit();

        assert_eq!(sensor.read(), 121.0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /** O ponto real desta mudança: `Sensor::new()` declara o `need` antes mesmo do componente que
    oferece a chave ter se inscrito — a ordem entre quem pede e quem oferece nunca importou pra
    `Proxy` (Art. 6.3), e agora também não importa mais pra `Sensor`/`ReadProxy`. Art. 3.8's antiga
    proibição (Sensor só depois do resolve() geral) não se aplica mais.
    */
    #[test]
    fn sensor_can_be_declared_before_its_key_is_offered() {
        let registry = StateRegistry::shared();

        let sensor = Sensor::new(&mut registry.borrow_mut(), "reactor.pressure", Box::new(Ideal));

        let (offered, _) = registry.borrow_mut().subscribe(&["reactor.pressure"], &[]);
        offered[0].set(2705.0);

        registry.borrow_mut().resolve().unwrap();
        registry.borrow_mut().commit();

        assert_eq!(sensor.read(), 2705.0);
    }

    /** Prova a invariante nova: `Sensor::new()` já registra o sensor no catálogo de
    `StateRegistry`, sob a própria `key` — ninguém precisa chamar `offer_sensor()` à parte, e nem
    poderia (é `pub(crate)`).
    */
    #[test]
    fn new_registers_itself_under_its_own_key() {
        let registry = StateRegistry::shared();
        let sensor = Sensor::new(&mut registry.borrow_mut(), "reactor.pressure", Box::new(Ideal));
        let sensor: Arc<dyn SensorTrait> = sensor;

        let found = registry.borrow().sensor("reactor.pressure").expect("deveria estar no catálogo");
        assert!(Arc::ptr_eq(&sensor, &found));
    }

    #[test]
    fn sensor_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Sensor>();
    }
}
