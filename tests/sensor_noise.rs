/* tests/sensor_noise.rs

Prova a opção `noise` de `#[monjolo::sensor(...)]` (issue spec-tennessee-eastman#66). Fica num
teste de integração, não em `component.rs`: um `#[sensor]` declarado lá se auto-registraria via
`inventory` no binário de testes da crate e quebraria o teste que descobre todos os componentes
(a chave deste sensor não teria provedor). Aqui é um binário à parte.
*/

use monjolo::sensor::Sensor as _;
use monjolo::state_registry::StateRegistry;

#[monjolo::sensor(key = "test.noise.ideal")]
struct IdealSensor;

#[monjolo::sensor(key = "test.noise.noisy", noise = 2.0)]
struct NoisySensor;

#[monjolo::sensor(key = "test.noise.seeded", noise = 2.0, seed = 42)]
struct SeededSensor;

const TRUE_VALUE: f64 = 100.0;

/* Oferece `key` com valor fixo e devolve `samples` leituras do sensor, uma por `commit()` — o
`Sensor` só sorteia ruído novo quando a geração muda (ver sensor/model.rs).
*/
fn sample<F>(key: &str, build: F, samples: usize) -> Vec<f64>
where
    F: FnOnce(&mut StateRegistry) -> std::sync::Arc<monjolo::sensor::model::Sensor>,
{
    let registry = StateRegistry::shared();
    let (offered, _) = registry.borrow_mut().subscribe(&[key], &[]);
    offered[0].set(TRUE_VALUE);
    let sensor = build(&mut registry.borrow_mut());
    registry.borrow_mut().resolve().expect("a chave tem provedor");

    (0..samples)
        .map(|_| {
            registry.borrow_mut().commit();
            sensor.read()
        })
        .collect()
}

fn mean_and_std(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

#[test]
fn without_noise_the_reading_is_exactly_the_physical_value() {
    let readings = sample("test.noise.ideal", |r| IdealSensor::new(r), 100);
    assert!(readings.iter().all(|v| *v == TRUE_VALUE), "Ideal não pode alterar o valor");
}

#[test]
fn noise_has_the_declared_standard_deviation_around_the_physical_value() {
    let readings = sample("test.noise.noisy", |r| NoisySensor::new(r), 20_000);
    let (mean, std) = mean_and_std(&readings);
    assert!((mean - TRUE_VALUE).abs() < 0.1, "média {mean} longe de {TRUE_VALUE}");
    assert!((std - 2.0).abs() < 0.1, "desvio padrão {std} longe de 2.0");
}

#[test]
fn noise_is_reproducible_from_run_to_run() {
    let a = sample("test.noise.noisy", |r| NoisySensor::new(r), 50);
    let b = sample("test.noise.noisy", |r| NoisySensor::new(r), 50);
    assert_eq!(a, b, "semente derivada da key deveria repetir a mesma sequência");
}

#[test]
fn explicit_seed_changes_the_sequence() {
    let derived = sample("test.noise.noisy", |r| NoisySensor::new(r), 50);
    let seeded = sample("test.noise.seeded", |r| SeededSensor::new(r), 50);
    assert_ne!(derived, seeded, "seed explícita deveria gerar outra sequência");
}
