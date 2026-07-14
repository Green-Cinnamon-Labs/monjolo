// src/adapter/snapshot_bus.rs
//
// Ponte thread-safe de LEITURA entre a "Thread da planta" e qualquer
// consumidor externo (ex.: a "Thread do OPC-UA") — ver
// drawio/dynamicModel.drawio, aba "arquitetura", nó "Sensor Snapshot Bus".
//
// A planta publica todas as leituras do tick aqui, de uma vez (`publish_all`);
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

    /** Chamado pela "Thread da planta" uma vez por tick, com todas as
    leituras do tick já resolvidas. Toma o `write()` lock uma única vez pra
    escrever todos os sensores na mesma seção crítica — ao contrário de
    publicar um-a-um, um leitor concorrente nunca vê uma mistura de valores
    de ticks diferentes entre variáveis distintas: o snapshot lido é sempre
    ou o tick anterior inteiro, ou o tick atual inteiro.
    */
    pub fn publish_all<'a>(&self, readings: impl IntoIterator<Item = (&'a str, f64)>) {
        let mut values = self
            .values
            .write()
            .expect("SnapshotBus: lock envenenado (uma thread escritora entrou em pânico)");
        for (name, value) in readings {
            values.insert(name.to_string(), value);
        }
    }

    /** Chamado por quem consome os valores publicados. `None` até o
    primeiro `publish_all()` que inclua aquele nome.
    */
    pub fn read(&self, name: &str) -> Option<f64> {
        self.values
            .read()
            .expect("SnapshotBus: lock envenenado (uma thread escritora entrou em pânico)")
            .get(name)
            .copied()
    }
}
