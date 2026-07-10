// src/adapter/mod.rs
//
// Adaptadores de rede — quem expõe sensores/atuadores da "Thread da planta"
// pro mundo de fora. Mesmo raciocínio de `numerical_method`: hoje só existe
// `opcua`, mas o desenho já é "um dentre N possíveis" (MQTT, REST, etc.
// poderiam entrar aqui do mesmo jeito no futuro).
//
// `command_queue`/`snapshot_bus` não são específicos de OPC-UA — são as
// duas pontes thread-safe genéricas (SnapshotBus de leitura, CommandQueue
// de escrita) que QUALQUER adapter usaria pra falar com a "Thread da
// planta" sem tocar StateRegistry direto. Por isso moram aqui, não dentro
// de `opcua.rs`.

pub mod command_queue;
#[cfg(feature = "opcua")]
pub mod opcua;
pub mod snapshot_bus;

/** Infraestrutura externa que `Simulation::run()` pode subir numa thread
própria — mesmo raciocínio de `NumericalMethod` (numerical_method/mod.rs):
um enum fechado, não um trait object aberto — `Simulation` só aceita o que
o framework já implementa aqui dentro.

Hoje só existe `OpcUa`, e a variante só existe com a feature `opcua`
ligada — sem a feature, o enum fica sem nenhum variante construível (`
Simulation::set_adapter()` continua compilando, só não há valor nenhum pra
passar pra ele).
*/
#[derive(Debug)]
pub enum AdapterConfig {
    #[cfg(feature = "opcua")]
    OpcUa { endpoint: String },
}
