/** Contrato mínimo de leitura. A implementação concreta genérica (`model`)
mora aqui mesmo — `Sensor`/`SensorBehavior`/ruído/histerese não têm nada de
específico do TEP, qualquer planta montada sobre `monjolo` reaproveita.

`&self`, não `&mut self`: implementações que precisam de mutabilidade
(ex.: RNG de ruído, cache de idempotência) resolvem isso com mutabilidade
interior — o trait não impõe como.
*/
pub trait Sensor {
    fn read(&self) -> f64;
}

pub mod model;
