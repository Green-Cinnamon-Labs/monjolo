// src/adapter/snapshot_bus.rs
//
// Ponte thread-safe de LEITURA entre a "Thread da planta" e qualquer
// consumidor externo (ex.: a "Thread do OPC-UA") — ver
// drawio/dynamicModel.drawio, aba "arquitetura", nó "Sensor Snapshot Bus".
//
// A planta publica o valor de cada sensor aqui a cada tick (`publish`);
// quem está do outro lado só lê (`read`) — nunca escreve. Só `std`, sem
// tokio: `Simulation::run()` usa isso incondicionalmente (mesmo sem a
// feature `opcua`), então não pode depender de nada opcional.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/** `Arc<RwLock<...>>` porque pode ter vários leitores concorrentes (ex.:
várias sessões OPC-UA lendo ao mesmo tempo) e um único escritor (a "Thread
da planta"). `Clone` é barato — todo clone aponta pro mesmo mapa.
*/
#[derive(Clone, Default)]
pub struct SnapshotBus {
    values: Arc<RwLock<HashMap<String, f64>>>,
}

impl SnapshotBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Chamado pela "Thread da planta", uma vez por sensor, a cada tick.
    pub fn publish(&self, name: &str, value: f64) {
        self.values
            .write()
            .expect("SnapshotBus: lock envenenado (uma thread escritora entrou em pânico)")
            .insert(name.to_string(), value);
    }

    /// Chamado por quem consome os valores publicados. `None` até o
    /// primeiro `publish()` daquele nome.
    pub fn read(&self, name: &str) -> Option<f64> {
        self.values
            .read()
            .expect("SnapshotBus: lock envenenado (uma thread escritora entrou em pânico)")
            .get(name)
            .copied()
    }
}
