/* monjolo/model_function.rs */

/* Documentação em `docs/model-function.md`; testes em `tests/model_function.rs`. */
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::component::{attach_discovered_components_where, ComponentKind};
use crate::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
use crate::snapshot::Snapshot;
use crate::state_registry::{Proxy, StateRegistry};

pub struct ModelFunction {
    root: Composite,
    /* Mantém vivos os catálogos (atuadores, instâncias) que os componentes de `root`
    referenciam.
    */
    _registry: Rc<RefCell<StateRegistry>>,
    /* Em ordem alfabética; `proxies` e `base` seguem a mesma ordem. */
    keys: Vec<String>,
    proxies: Vec<Proxy>,
    base: Vec<f64>,
    index: HashMap<String, usize>,
    state_keys: Vec<String>,
}

impl ModelFunction {
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

    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn state_keys(&self) -> &[String] {
        &self.state_keys
    }

    pub fn base(&self, key: &str) -> Option<f64> {
        self.index.get(key).map(|&slot| self.base[slot])
    }

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
