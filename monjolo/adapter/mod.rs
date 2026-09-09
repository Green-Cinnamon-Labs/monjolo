/** src/adapter/mod.rs

Adaptadores de rede — quem expõe sensores/atuadores da "Thread da planta" pro mundo de fora. Mesmo
raciocínio de `numerical_method`: hoje só existe `opcua`, mas o desenho já é "um dentre N possíveis"
(MQTT, REST, etc. poderiam entrar aqui do mesmo jeito no futuro).

Não existe mais nenhuma ponte própria aqui, nem de leitura nem de escrita
(IoImage/CommandSink/CommandQueue eliminados): `Sensor` e `Actuator` (actuator/model.rs) são ambos
`Send + Sync` e são exportados direto, via `Arc`, no handshake de boot (`ready_tx`, `simulation.rs`)
— qualquer adapter lê `sensor.read()`/escreve `actuator.write()` sem intermediário.
*/

#[cfg(feature = "opcua")]
pub mod opcua;

#[cfg(feature = "opcua")]
use std::sync::Arc;

#[cfg(feature = "opcua")]
use crate::runtime_control::RuntimeControl;

/** Infraestrutura externa que `Simulation::run()` pode subir numa thread própria — mesmo raciocínio
de `NumericalMethod` (numerical_method/mod.rs): um enum fechado, não um trait object aberto —
`Simulation` só aceita o que o framework já implementa aqui dentro.

Hoje só existe `OpcUa`, e a variante só existe com a feature `opcua` ligada — sem a feature, o enum
fica sem nenhum variante construível (`Simulation::set_adapter()` continua compilando, só não há
valor nenhum pra passar pra ele).
*/
#[derive(Debug)]
pub enum AdapterConfig {
    /* `control`: mesmo `Arc<RuntimeControl>` que `Simulation::runtime_control()` devolve — quem
    monta o adapter (ex.: `tep-plant/src/main.rs`) passa a MESMA instância que também vai pra dentro
    da Thread da planta, não uma cópia independente (`RuntimeControl` não tem "duas fontes da
    verdade": um único `Arc` compartilhado nos dois sentidos). */
    #[cfg(feature = "opcua")]
    OpcUa {
        endpoint: String,
        control: Arc<RuntimeControl>,
    },
}
