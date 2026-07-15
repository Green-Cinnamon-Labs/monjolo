// src/adapter/mod.rs
//
// Adaptadores de rede — quem expõe sensores/atuadores da "Thread da planta"
// pro mundo de fora. Mesmo raciocínio de `numerical_method`: hoje só existe
// `opcua`, mas o desenho já é "um dentre N possíveis" (MQTT, REST, etc.
// poderiam entrar aqui do mesmo jeito no futuro).
//
// `command_queue` não é específico de OPC-UA — é a ponte thread-safe
// genérica (escrita) que QUALQUER adapter usaria pra falar com a "Thread da
// planta" sem tocar StateRegistry direto. Por isso mora aqui, não dentro de
// `opcua.rs`. Do lado da leitura não existe mais ponte nenhuma (Art. 11.4/3.6.6
// do plano legislativo, 2026-07-15): `Sensor` é `Send + Sync` e é exportado
// direto, via `Arc`, no handshake de boot (`ready_tx`, `simulation.rs`) —
// qualquer adapter lê `sensor.read()` sem intermediário.

pub mod command_queue;
#[cfg(feature = "opcua")]
pub mod opcua;

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
