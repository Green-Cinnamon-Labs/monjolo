/** src/adapter/mod.rs

Adaptadores de rede — quem expõe sensores/atuadores da planta pro mundo de fora. Mesmo raciocínio de
`numerical_method`: hoje só existe `opcua`, mas o desenho já é "um dentre N possíveis" (MQTT, REST,
etc. poderiam entrar aqui do mesmo jeito no futuro).

NOTA (2026-09-09, issue #67): `AdapterConfig` (enum fechado de configuração de adapter, com um campo
`control: Arc<RuntimeControl>` fixo) foi removido — fazia sentido quando um adapter apontava pra UMA
`Simulation` fixa pelo tempo de vida inteiro do processo; agora que `crate::runtime::Runtime` troca
atomicamente pra qual `Simulation` o adapter aponta a cada `reset()`, não há mais "o `RuntimeControl`
desta configuração" pra guardar — cada adapter lê `Runtime::binding()` fresco a cada tick/callback.
Cada adaptador ganha seu próprio método dedicado em `Runtime` (ex.: `Runtime::spawn_opcua_adapter`),
em vez de um enum compartilhado — configs de adapter tendem a divergir bastante entre protocolos
(porta OPC-UA vs. tópico MQTT vs. rota REST), então um método por adaptador, cada um com sua própria
assinatura, é mais direto do que um enum crescendo um campo por variante.

Não existe nenhuma ponte própria aqui, nem de leitura nem de escrita (IoImage/CommandSink/
CommandQueue eliminados): `Sensor` e `Actuator` são ambos `Send + Sync` e são exportados direto, via
`Arc`, dentro de `crate::simulation::PlantBinding` — qualquer adapter lê `sensor.read()`/escreve via
o canal de comandos sem intermediário.
*/

#[cfg(feature = "opcua")]
pub mod opcua;
