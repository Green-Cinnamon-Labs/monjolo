/** Contrato mínimo de leitura. A implementação concreta genérica (`model`) mora aqui mesmo —
`Sensor`/`SensorBehavior`/ruído/histerese não têm nada de específico do TEP, qualquer planta montada
sobre `monjolo` reaproveita.

`&self`, não `&mut self`: implementações que precisam de mutabilidade (ex.: RNG de ruído, cache de
idempotência) resolvem isso com mutabilidade interior — o trait não impõe como.

`Send + Sync` como supertrait — não decoração: `StateRegistry` cataloga `Arc<dyn Sensor>`
(`sensor_catalog`) especificamente pra que um consumidor de outra thread (ex.: adapter OPC-UA,
`adapter/opcua.rs`) possa segurar o mesmo objeto sem bridge nenhuma. Sem o supertrait, `dyn Sensor`
não seria `Send`/`Sync` mesmo com o único implementor real (`sensor::model::Sensor`) já sendo os
dois — a garantia do tipo concreto não "sobe" pro trait object sozinha.
*/
pub trait Sensor: Send + Sync {
    fn read(&self) -> f64;
}

pub mod model;
