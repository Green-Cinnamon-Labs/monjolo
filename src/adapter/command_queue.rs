// src/adapter/command_queue.rs
//
// Ponte thread-safe de ESCRITA — a única ponte que sobrou entre a "Thread
// da planta" e a "Thread do adapter" depois da eliminação do SnapshotBus
// (Art. 11.4 do plano legislativo, 2026-07-15: leitura de sensor passou a
// ser direta, via Arc<Sensor>, Art. 3.6.6). Ver drawio/dynamicModel.drawio, aba
// "arquitetura", nó "Command Queue".
//
// Quem está fora da "Thread da planta" (ex.: um write callback do OPC-UA,
// chamado de qualquer thread/tarefa que o servidor decidir usar) empurra
// `(nome, valor)` aqui; a "Thread da planta" drena no início de cada tick e
// aplica via `IoImage::write()` — nunca o contrário, nunca escrita direta
// no `StateRegistry` por fora do ciclo tick/commit.
//
// `std::sync::mpsc::Sender` sozinho é `Send` mas não `Sync` — um write
// callback do async-opcua exige `Fn(...) + Send + Sync + 'static`. Por isso
// o `Sender` fica atrás de `Arc<Mutex<_>>`: o `Mutex` doa o `Sync` que falta
// (o lock só disputa em escritas de atuador, que são raras — sem custo real
// no caminho quente da simulação).

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct CommandQueue {
    sender: Arc<Mutex<Sender<(String, f64)>>>,
}

impl CommandQueue {
    pub fn new(sender: Sender<(String, f64)>) -> Self {
        Self {
            sender: Arc::new(Mutex::new(sender)),
        }
    }

    /// Enfileira um comando — não bloqueia esperando a planta processar.
    /// Silencioso se o receiver já foi descartado (planta encerrada).
    pub fn write(&self, name: &str, value: f64) {
        let _ = self
            .sender
            .lock()
            .expect("CommandQueue: lock envenenado (uma thread escritora entrou em pânico)")
            .send((name.to_string(), value));
    }
}
