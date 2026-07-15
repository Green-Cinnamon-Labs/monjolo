// io_image.rs

/** Fronteira externa mínima do monjolo (ver
 docs/issue55_opcua_refactor/plan_refactor.md, seção 10): o lugar único
 onde Sensores publicam valores e Atuadores recebem comandos, por nome
 semântico — análogo a uma imagem de I/O/tabela de tags/registradores
 numa planta real.

 Não sabe nada de DynamicModel, RK4, StateRegistry ou avaliação hipotética
 — só embrulha `Sensor` (leitura) e um sink de comando genérico (escrita)
 atrás de um catálogo nomeado. É a interface que um adaptador futuro
 (OPC-UA, Modbus/register-map, HTTP/gRPC, WebSocket...) vai consumir;
 nenhum desses existe ainda — hoje só existe este adaptador interno, em
 memória, tosco de propósito.
*/
use std::collections::HashMap;
use std::sync::Arc;

use crate::sensor::model::Sensor;

/** Sink de comando genérico — a escrita equivalente ao `Sensor` de leitura.
Qualquer coisa que aceite um valor (`Valve::set_command`,
`Agitator::set_command`, futuramente a entrada de um Controlador) vira um
`CommandSink` por fechamento (closure), sem que a `IoImage` precise conhecer
o tipo concreto por trás.
*/
pub trait CommandSink {
    fn write(&mut self, value: f64);
}

impl<F: FnMut(f64)> CommandSink for F {
    fn write(&mut self, value: f64) {
        self(value)
    }
}

/** Deixa um `Box<dyn CommandSink + Send>` já resolvido (ex.: a especificação
guardada por `Simulation` antes de `run()`) ser aceito onde
`register_actuator` espera `impl CommandSink` — só encaminha pro sink de
dentro. Fica double-boxed (`Box<Box<dyn CommandSink + Send>>` por trás),
aceitável porque escrita de atuador não é caminho quente.
*/
impl CommandSink for Box<dyn CommandSink + Send> {
    fn write(&mut self, value: f64) {
        (**self).write(value)
    }
}

/** Catálogo central de sinais — a I/O Image. Sensores entram como sinais de
leitura (convenção: `sensors/<nome>`), atuadores como sinais de escrita
(convenção: `actuators/<nome>`) — o prefixo é só convenção do chamador, não
é imposto pelo tipo. Leitura e escrita são catálogos separados: nada impede
"sensors/x" e "actuators/x" coexistirem.
*/
#[derive(Default)]
pub struct IoImage {
    readable: HashMap<String, Arc<Sensor>>,
    writable: HashMap<String, Box<dyn CommandSink>>,
}

impl IoImage {
    pub fn new() -> Self {
        Self::default()
    }

    /** Registra um `Sensor` já construído sob um nome, como `Arc` — não
    constrói o Sensor aqui; quem já resolveu a chave contra o
    `StateRegistry` (seção 3.8 do plano) entrega o `Sensor` pronto;
    `IoImage` não sabe nada de `StateRegistry`/`ReadProxy`. `Arc` (em vez de
    posse exclusiva) porque o mesmo `Sensor` também é exportado pro
    catálogo que atravessa pra Thread do Adaptador (`sensor_catalog()`,
    Art. 11.4/11.8 do plano legislativo) — nunca é copiado, só compartilhado.
    */
    pub fn register_sensor(&mut self, name: &str, sensor: Arc<Sensor>) {
        self.readable.insert(name.to_string(), sensor);
    }

    /** Catálogo de sensores registrados, pronto pra exportar — cada clone
    só incrementa o refcount do `Arc` (barato), nunca duplica `Sensor`/
    `SensorBehavior`. Usado uma única vez, no handshake de boot (Art. 11.8/
    11.9), pra dar à Thread do Adaptador (e a um futuro Controlador) acesso
    direto a `sensor.read()` — sem depender da `IoImage` nem de nenhum
    canal de publicação.
    */
    pub fn sensor_catalog(&self) -> HashMap<String, Arc<Sensor>> {
        self.readable.clone()
    }

    /** Registra um sink de comando sob um nome. Ex.:
    `io.register_actuator("actuators/cooling_water.command", move |v| valve.set_command(v))`.
    */
    pub fn register_actuator(&mut self, name: &str, sink: impl CommandSink + 'static) {
        self.writable.insert(name.to_string(), Box::new(sink));
    }

    /// Lista todos os sinais conhecidos, por nome — leitura e escrita
    /// juntos. Só o catálogo (debug/introspecção); não diz o valor.
    pub fn signals(&self) -> Vec<&str> {
        self.readable
            .keys()
            .chain(self.writable.keys())
            .map(String::as_str)
            .collect()
    }

    /// Nomes dos sinais de leitura (sensores publicados). Usado por um
    /// adaptador externo (ex.: `opcua_adapter`) pra saber quais nodes
    /// read-only criar — não distingue isso a partir de `signals()` porque
    /// leitura/escrita são catálogos internos separados.
    pub fn sensor_names(&self) -> impl Iterator<Item = &str> {
        self.readable.keys().map(String::as_str)
    }

    /// Nomes dos sinais de escrita (atuadores registrados).
    pub fn actuator_names(&self) -> impl Iterator<Item = &str> {
        self.writable.keys().map(String::as_str)
    }

    /** Lê o valor atual de um sinal de leitura por nome. `None` se o nome
    não existe ou não é um sinal de leitura. `&self`, não `&mut self`:
    `Sensor::read()` (Art. 3.6.6) é `&self` — a mutação de `SensorBehavior`
    fica atrás do `Mutex` interno do próprio `Sensor`.
    */
    pub fn read(&self, name: &str) -> Option<f64> {
        self.readable.get(name).map(|sensor| sensor.read())
    }

    /// Escreve um comando num sinal de escrita por nome. `Err` se o nome
    /// não existe ou não é um sinal de escrita.
    pub fn write(&mut self, name: &str, value: f64) -> Result<(), String> {
        match self.writable.get_mut(name) {
            Some(sink) => {
                sink.write(value);
                Ok(())
            }
            None => Err(format!("IoImage: sinal de escrita '{name}' não existe")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensor::model::Ideal;
    use crate::state_registry::StateRegistry;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn registers_sensor_and_exposes_read() {
        let registry = StateRegistry::shared();
        let (offered, _) = registry
            .borrow_mut()
            .subscribe(&["reactor.temperature"], &[]);
        offered[0].set(120.5);
        registry.borrow_mut().resolve().unwrap();
        registry.borrow_mut().commit();

        let sensor = Sensor::new(registry, "reactor.temperature", Box::new(Ideal)).unwrap();
        let mut io = IoImage::new();
        io.register_sensor("sensors/reactor.temperature", Arc::new(sensor));

        assert_eq!(io.read("sensors/reactor.temperature"), Some(120.5));
        assert_eq!(io.read("sensors/does.not.exist"), None);
    }

    #[test]
    fn registers_actuator_and_writes_command() {
        let received = Rc::new(RefCell::new(0.0));
        let received_clone = received.clone();
        let mut io = IoImage::new();
        io.register_actuator("actuators/cooling_water.command", move |v| {
            *received_clone.borrow_mut() = v;
        });

        io.write("actuators/cooling_water.command", 42.0).unwrap();
        assert_eq!(*received.borrow(), 42.0);
        assert!(io.write("actuators/does.not.exist", 1.0).is_err());
    }

    #[test]
    fn signals_lists_both_readable_and_writable() {
        let registry = StateRegistry::shared();
        let (offered, _) = registry
            .borrow_mut()
            .subscribe(&["reactor.temperature"], &[]);
        offered[0].set(1.0);
        registry.borrow_mut().resolve().unwrap();
        registry.borrow_mut().commit();

        let sensor = Sensor::new(registry, "reactor.temperature", Box::new(Ideal)).unwrap();
        let mut io = IoImage::new();
        io.register_sensor("sensors/reactor.temperature", Arc::new(sensor));
        io.register_actuator("actuators/cooling_water.command", |_v| {});

        let mut signals = io.signals();
        signals.sort();
        assert_eq!(
            signals,
            vec![
                "actuators/cooling_water.command",
                "sensors/reactor.temperature"
            ]
        );
    }
}
