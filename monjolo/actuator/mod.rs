/** Contrato mínimo de escrita — a contraparte de `Sensor` (`sensor/mod.rs`). A implementação
concreta genérica (`model`) mora aqui mesmo: um atuador de estado único com dinâmica de 1ª ordem
arbitrária, fornecida por quem constrói via closure — não há nada de específico de planta nenhuma
aqui, e não há mais um tipo Rust por atuador físico (`tep-plant` não declara mais `struct FeedDValve`
etc.: cada válvula é só uma instância de `actuator::model::Actuator` com sua própria chave/lei).

Um `DynamicModel` pode perfeitamente também implementar `Actuator` — não há conflito: são dois
traits distintos sobre o mesmo objeto (é exatamente o que `actuator::model::Actuator` faz).
*/
pub trait Actuator {
    fn write(&self, value: f64);
}

pub mod model;
