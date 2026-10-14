/* tests/model_function.rs

Prova `monjolo::model_function::ModelFunction` (spec-tennessee-eastman#92) só pela interface
pública, com modelos de brinquedo montados à mão. O que a função garante está descrito em
`docs/model-function.md`.
*/

use std::cell::Cell;

use monjolo::dynamic_model::DynamicModel;
use monjolo::model_function::ModelFunction;
use monjolo::snapshot::Snapshot;
use monjolo::state_registry::{Proxy, StateRegistry};

const RATE: f64 = 0.5;

/* dv/dt = -RATE·v, mais uma grandeza derivada (`toy.double = 2v`) — o mínimo pra ter um estado,
uma derivada e uma chave que só existe como resultado da avaliação.
*/
struct Decay {
    value: Proxy,
    derivative: Proxy,
    double: Proxy,
}

impl Decay {
    fn new(registry: &mut StateRegistry, config: &Snapshot) -> Self {
        let (offered, _) =
            registry.subscribe(&["toy.value", "toy.value.derivative", "toy.double"], &[]);
        offered[0].set(config.get("toy.value").unwrap_or(0.0));
        Self {
            value: offered[0].clone(),
            derivative: offered[1].clone(),
            double: offered[2].clone(),
        }
    }
}

impl DynamicModel for Decay {
    fn evaluate(&self) {
        let value = self.value.get();
        self.derivative.set(-RATE * value);
        self.double.set(2.0 * value);
    }

    fn state_keys(&self) -> Vec<String> {
        vec!["toy.value".to_string()]
    }
}

fn decay_function() -> ModelFunction {
    let config = Snapshot::from_pairs(&[("toy.value", 10.0)]);
    ModelFunction::new(&config, Decay::new).expect("Decay é uma função do ponto")
}

#[test]
fn lists_keys_states_and_base_values() {
    let function = decay_function();

    assert_eq!(function.keys(), ["toy.double", "toy.value", "toy.value.derivative"]);
    assert_eq!(function.state_keys(), ["toy.value"]);
    assert_eq!(function.base("toy.value"), Some(10.0));
    assert_eq!(function.base("toy.double"), Some(20.0));
    assert_eq!(function.base("toy.value.derivative"), Some(-5.0));
    assert_eq!(function.base("missing"), None);
}

#[test]
fn evaluates_the_analytic_derivative_at_a_written_point() {
    let function = decay_function();

    let result = function
        .evaluate(&[("toy.value", 3.0)], &["toy.value.derivative", "toy.double", "toy.value"])
        .unwrap();

    assert_eq!(result, [-RATE * 3.0, 6.0, 3.0]);
}

/* O teste de memória: o resultado de A não pode depender de B ter sido avaliado no meio. */
#[test]
fn point_a_gives_the_same_result_before_and_after_point_b() {
    let function = decay_function();
    let reads = ["toy.value.derivative", "toy.double"];

    let first = function.evaluate(&[("toy.value", 3.0)], &reads).unwrap();
    let _ = function.evaluate(&[("toy.value", 700.0)], &reads).unwrap();
    let again = function.evaluate(&[("toy.value", 3.0)], &reads).unwrap();

    assert_eq!(first, again);
}

/* Uma escrita vale só pra própria chamada: a seguinte volta a partir do ponto base. */
#[test]
fn a_write_does_not_survive_into_the_next_evaluation() {
    let function = decay_function();

    let _ = function.evaluate(&[("toy.value", 3.0)], &[]).unwrap();
    let at_base = function.evaluate(&[], &["toy.value", "toy.double"]).unwrap();

    assert_eq!(at_base, [10.0, 20.0]);
}

#[test]
fn unknown_keys_are_an_error_on_either_side() {
    let function = decay_function();

    assert!(function.evaluate(&[("toy.missing", 1.0)], &[]).unwrap_err().contains("toy.missing"));
    assert!(function.evaluate(&[], &["toy.missing"]).unwrap_err().contains("toy.missing"));
}

/* Estado declarado sem ninguém oferecendo a derivada — o mesmo erro que `Simulation` acusaria em
`resolve()`, só que aqui ninguém pede a derivada como `need`.
*/
struct StateWithoutDerivative;

impl DynamicModel for StateWithoutDerivative {
    fn evaluate(&self) {}

    fn state_keys(&self) -> Vec<String> {
        vec!["toy.orphan".to_string()]
    }
}

#[test]
fn a_state_without_a_derivative_key_is_rejected() {
    let config = Snapshot::from_pairs(&[]);
    let result = ModelFunction::new(&config, |registry, _config| {
        registry.subscribe(&["toy.orphan"], &[]);
        StateWithoutDerivative
    });

    assert!(result.err().expect("deveria recusar").contains("toy.orphan.derivative"));
}

/* Lê `toy.late` ANTES de recalculá-la na mesma avaliação: o valor publicado em `toy.early` fica um
passo atrasado, então avaliar duas vezes o mesmo ponto dá resultados diferentes.
*/
struct ReadsBeforeRecomputing {
    value: Proxy,
    early: Proxy,
    late: Proxy,
}

impl DynamicModel for ReadsBeforeRecomputing {
    fn evaluate(&self) {
        self.early.set(self.late.get());
        self.late.set(self.value.get() + 1.0);
    }
}

#[test]
fn a_model_that_changes_on_a_second_evaluation_is_rejected() {
    let config = Snapshot::from_pairs(&[]);
    let result = ModelFunction::new(&config, |registry, _config| {
        let (offered, _) = registry.subscribe(&["toy.value", "toy.early", "toy.late"], &[]);
        ReadsBeforeRecomputing {
            value: offered[0].clone(),
            early: offered[1].clone(),
            late: offered[2].clone(),
        }
    });

    assert!(result.err().expect("deveria recusar").contains("toy.early"));
}

/* Guarda o maior valor já visto num `Cell` próprio, fora do registry. Repete o resultado quando o
mesmo ponto é avaliado duas vezes seguidas (passa pela checagem da montagem), mas carrega memória de
outro ponto — só o teste A, B, A pega.
*/
struct RemembersThePeak {
    value: Proxy,
    peak: Proxy,
    seen: Cell<f64>,
}

impl DynamicModel for RemembersThePeak {
    fn evaluate(&self) {
        self.seen.set(self.seen.get().max(self.value.get()));
        self.peak.set(self.seen.get());
    }
}

#[test]
fn the_a_b_a_test_catches_memory_kept_outside_the_registry() {
    let config = Snapshot::from_pairs(&[]);
    let function = ModelFunction::new(&config, |registry, _config| {
        let (offered, _) = registry.subscribe(&["toy.value", "toy.peak"], &[]);
        RemembersThePeak {
            value: offered[0].clone(),
            peak: offered[1].clone(),
            seen: Cell::new(0.0),
        }
    })
    .expect("passa pela checagem da montagem: o mesmo ponto repete o mesmo valor");

    let first = function.evaluate(&[("toy.value", 3.0)], &["toy.peak"]).unwrap();
    let _ = function.evaluate(&[("toy.value", 700.0)], &["toy.peak"]).unwrap();
    let again = function.evaluate(&[("toy.value", 3.0)], &["toy.peak"]).unwrap();

    assert_ne!(first, again, "A, B, A deveria denunciar a memória deste modelo");
}
