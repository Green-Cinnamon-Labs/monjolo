/* monjolo/model_function.rs */

/** O modelo como FUNÇÃO, fora do laço de simulação (spec-tennessee-eastman#91/#92): escrever valores
em chaves do `StateRegistry`, avaliar a árvore e ler outras chaves de volta. É o que um otimizador
precisa — derivadas e medições num ponto qualquer, sem tempo, sem integração e sem thread.

Toda chave oferecida no registry pode ser lida. Escrever só tem efeito numa chave que nenhum
componente recalcula durante `evaluate()` (um estado integrável, o liga/desliga de um distúrbio): o
resto é sobrescrito pela própria avaliação.

Cada avaliação parte do PONTO BASE — o conteúdo inteiro do `EvaluationState` logo depois da montagem,
com os estados semeados pelo `Snapshot` — e só então aplica as escritas pedidas. É isso que faz o
resultado depender só do que foi pedido na própria chamada: sem essa restauração, um estado escrito
numa chamada continuaria valendo na seguinte.

A restauração não cobre o que um componente guarda FORA do registry (um `Cell` próprio). Por isso a
montagem também recusa um modelo em que avaliar duas vezes seguidas o mesmo ponto muda algum valor
— sinal de que alguma chave é lida antes de ser recalculada, ou de que algo acumula entre chamadas.
Isso não substitui o teste A, B, A de quem usa: um componente pode repetir o mesmo valor no mesmo
ponto e ainda assim carregar memória de outro ponto.

Sem `Simulation` e sem `commit()`: `Sensor` (que lê `CurrentState`) e `Controller` (que lê `Sensor`)
não participam de `discovered()`.
*/
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::component::{attach_discovered_components_where, ComponentKind};
use crate::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
use crate::snapshot::Snapshot;
use crate::state_registry::{Proxy, StateRegistry};

pub struct ModelFunction {
    root: Composite,
    /* Mantém vivos os catálogos (atuadores, instâncias) que os componentes de `root` referenciam. */
    _registry: Rc<RefCell<StateRegistry>>,
    /* Em ordem alfabética; `proxies` e `base` seguem a mesma ordem. */
    keys: Vec<String>,
    proxies: Vec<Proxy>,
    base: Vec<f64>,
    index: HashMap<String, usize>,
    state_keys: Vec<String>,
}

impl ModelFunction {
    /** Monta a função a partir de um modelo construído à mão — `build` recebe o registry e o
    `Snapshot` de config, mesma assinatura da fábrica de `Simulation::set_model()`.
    */
    pub fn new<M>(
        config: &Snapshot,
        build: impl FnOnce(&mut StateRegistry, &Snapshot) -> M,
    ) -> Result<Self, String>
    where
        M: DynamicModel + 'static,
    {
        let registry = StateRegistry::shared();
        let model = build(&mut registry.borrow_mut(), config);
        let mut root = Composite::new();
        root.add_dynamic(Box::new(model));
        Self::assemble(registry, root)
    }

    /** Monta a função só com o que `inventory` descobre (mesma descoberta de `Simulation`), deixando
    de fora `Controller` e `Sensor`: quem chama escreve direto o que um controlador comandaria, e lê
    as grandezas cruas do registry, sem o ruído que só existe dentro de um `Sensor`.
    */
    pub fn discovered(config: &Snapshot) -> Result<Self, String> {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        attach_discovered_components_where(&mut root, &mut registry.borrow_mut(), config, |kind| {
            !matches!(kind, ComponentKind::Controller | ComponentKind::Sensor)
        });
        Self::assemble(registry, root)
    }

    fn assemble(registry: Rc<RefCell<StateRegistry>>, root: Composite) -> Result<Self, String> {
        let state_keys = root.state_keys();
        let mut keys: Vec<String> = registry.borrow().keys().map(String::from).collect();
        keys.sort();

        for state_key in &state_keys {
            let derivative_key = format!("{state_key}.derivative");
            if keys.binary_search(&derivative_key).is_err() {
                return Err(format!(
                    "estado '{state_key}' declarado em state_keys() mas ninguém oferece '{derivative_key}'"
                ));
            }
        }

        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let (_, proxies) = registry.borrow_mut().subscribe(&[], &key_refs);
        registry.borrow_mut().resolve()?;

        root.evaluate();
        let base: Vec<f64> = proxies.iter().map(Proxy::get).collect();

        root.evaluate();
        let changed: Vec<String> = keys
            .iter()
            .zip(&proxies)
            .zip(&base)
            .filter(|((_, proxy), &first)| proxy.get().to_bits() != first.to_bits())
            .map(|((key, proxy), &first)| format!("{key} ({first} -> {})", proxy.get()))
            .collect();
        if !changed.is_empty() {
            return Err(format!(
                "o modelo não é uma função do ponto: avaliar duas vezes seguidas mudou {} chave(s): {}",
                changed.len(),
                changed.join(", "),
            ));
        }

        let index = keys.iter().enumerate().map(|(slot, key)| (key.clone(), slot)).collect();
        Ok(Self {
            root,
            _registry: registry,
            keys,
            proxies,
            base,
            index,
            state_keys,
        })
    }

    /* Todas as chaves do registry, em ordem alfabética. */
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /* As chaves que o `Integrator` avançaria — cada `K` aqui tem uma `K.derivative` em `keys()`. */
    pub fn state_keys(&self) -> &[String] {
        &self.state_keys
    }

    /* Valor de uma chave no ponto base. `None` se a chave não existe. */
    pub fn base(&self, key: &str) -> Option<f64> {
        self.index.get(key).map(|&slot| self.base[slot])
    }

    /** Avalia o modelo no ponto base com `writes` aplicado por cima, e devolve o valor de cada chave
    de `reads`, na mesma ordem. `Err` se alguma chave não existir — nesse caso nada é avaliado.
    */
    pub fn evaluate(&self, writes: &[(&str, f64)], reads: &[&str]) -> Result<Vec<f64>, String> {
        let mut write_slots = Vec::with_capacity(writes.len());
        for &(key, value) in writes {
            write_slots.push((self.slot(key)?, value));
        }
        let mut read_slots = Vec::with_capacity(reads.len());
        for &key in reads {
            read_slots.push(self.slot(key)?);
        }

        for (proxy, &value) in self.proxies.iter().zip(&self.base) {
            proxy.set(value);
        }
        for (slot, value) in write_slots {
            self.proxies[slot].set(value);
        }
        self.root.evaluate();

        Ok(read_slots.into_iter().map(|slot| self.proxies[slot].get()).collect())
    }

    fn slot(&self, key: &str) -> Result<usize, String> {
        self.index.get(key).copied().ok_or_else(|| format!("chave desconhecida: '{key}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

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

    /* Estado declarado sem ninguém oferecendo a derivada — o mesmo erro que `Simulation` acusaria
    em `resolve()`, só que aqui ninguém pede a derivada como `need`.
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

    /* Lê `toy.late` ANTES de recalculá-la na mesma avaliação: o valor publicado em `toy.early`
    fica um passo atrasado, então avaliar duas vezes o mesmo ponto dá resultados diferentes.
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

    /* Guarda o maior valor já visto num `Cell` próprio, fora do registry. Repete o resultado quando
    o mesmo ponto é avaliado duas vezes seguidas (passa pela checagem da montagem), mas carrega
    memória de outro ponto — só o teste A, B, A pega.
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
}
