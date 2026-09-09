/* monjolo/runtime_control.rs */

/** Alça de controle ao vivo da "Thread da planta", enquanto ela roda — o que faltava desde que
`Simulation::run()` virou bloqueante (ver nota da Art. 11.10 do CONTRIBUTING: "não há cancelamento
cooperativo"). `Send + Sync` de verdade (só atomics por dentro, nenhum `Rc`/`RefCell`), então
atravessa pra qualquer adapter sem bridge — mesmo raciocínio de `Sensor`/`Actuator` (Art. 3.6.6/
12.1): construído fora da Thread da planta (em `Simulation::new()`), uma cópia do `Arc` vai pra
dentro dela (lida a cada tick) e outra pra fora, pra quem monta o adapter (ex.:
`tep-plant/src/main.rs` → `AdapterConfig::OpcUa { control, .. }`), sem canal em nenhum dos dois
sentidos.

Diferente de `Sensor`/`Actuator`: não é algo que o usuário do framework declara por instância (não há
`#[runtime_control(...)]`) — existe exatamente um por `Simulation`, então não precisa do mecanismo de
catálogo/`inventory` que sensores/atuadores usam.

NOTA: `reset` ainda não tem Method OPC-UA nem tratamento na Thread da planta — `request_reset()`/
`take_reset_request()` existem como interface, mas reconstruir o modelo/estado físico do zero exige
que `model_factory` (hoje `FnOnce`, `simulation.rs`) vire re-invocável, uma mudança maior, ainda em
aberto (ver discussão da issue #61/#66).
*/

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/** Handle Send+Sync pra pausar/retomar/ajustar o fator de velocidade da simulação de fora da Thread
da planta, além de ler o tempo simulado acumulado (`t_h`). Nenhum campo é lido/escrito diretamente
por fora deste arquivo — só via os métodos abaixo, todos com `Ordering::Relaxed`: os flags/valores
aqui são independentes entre si (nenhum protege o acesso a outro dado), não há relação de
happens-before a preservar, só a atomicidade de cada leitura/escrita individual.
*/
#[derive(Debug)]
pub struct RuntimeControl {
    paused: AtomicBool,
    reset_requested: AtomicBool,
    /* f64 não tem variante atômica nativa — bit-cast via `to_bits()`/`from_bits()`, o mesmo truque
    de sempre pra um valor de ponto flutuante atravessar threads sem lock. */
    speed_factor: AtomicU64,
    t_h: AtomicU64,
}

impl Default for RuntimeControl {
    fn default() -> Self {
        Self {
            paused: AtomicBool::new(false),
            reset_requested: AtomicBool::new(false),
            speed_factor: AtomicU64::new(1.0f64.to_bits()),
            t_h: AtomicU64::new(0.0f64.to_bits()),
        }
    }
}

impl RuntimeControl {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Chamado por quem está de fora da Thread da planta (ex.: um Method do adaptador OPC-UA) ──

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    /** Só marca o pedido — a Thread da planta é quem decide o que "reset" significa de verdade e
    consome esse flag via `take_reset_request()`. Ver nota no topo do arquivo: hoje nada consome
    este flag ainda. */
    pub fn request_reset(&self) {
        self.reset_requested.store(true, Ordering::Relaxed);
    }

    /** 0.0 = o mais rápido possível (sem dormir entre ticks); 1.0 = tempo real; N = Nx. Negativo
    vira 0.0 — não existe "velocidade negativa" nesta simulação. */
    pub fn set_speed(&self, factor: f64) {
        self.speed_factor.store(factor.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn speed(&self) -> f64 {
        f64::from_bits(self.speed_factor.load(Ordering::Relaxed))
    }

    /** Tempo simulado acumulado, em horas — soma de `dt_hours` a cada tick já commitado. Não é lido
    via `StateRegistry`/`Sensor` (Art. 3.6.6) porque não é uma grandeza física de nenhum componente:
    é uma propriedade da própria orquestração (`Simulation`), então vive aqui, ao lado das outras
    alças de controle ao vivo — o adaptador OPC-UA publica isso como mais um node de leitura, direto
    (ver `adapter/opcua.rs`), sem precisar embrulhar num `Sensor`. */
    pub fn t_h(&self) -> f64 {
        f64::from_bits(self.t_h.load(Ordering::Relaxed))
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    // ── Chamado só pela Thread da planta, dentro de spawn_plant_thread ──

    /** Consome o pedido de reset — chamado no máximo uma vez por tick pela Thread da planta; troca
    por `false` atomicamente, então duas leituras nunca consomem o mesmo pedido duas vezes.
    `#[allow(dead_code)]`: nada chama isto ainda (ver nota no topo do arquivo). */
    #[allow(dead_code)]
    pub(crate) fn take_reset_request(&self) -> bool {
        self.reset_requested.swap(false, Ordering::Relaxed)
    }

    /** Chamado exatamente uma vez por tick, sempre pela mesma (única) Thread da planta — nunca há
    duas chamadas concorrentes, então o load-then-store não-atômico-como-um-todo abaixo é seguro
    (não existe torn write possível sem um segundo escritor). */
    pub(crate) fn advance_t_h(&self, dt_hours: f64) {
        let current = self.t_h();
        self.t_h.store((current + dt_hours).to_bits(), Ordering::Relaxed);
    }
}

/** `t_h` não é grandeza de nenhum componente físico (Art. 3.6.6 do CONTRIBUTING: `Sensor` observa
`StateRegistry`/`CurrentState`) — é propriedade da própria orquestração, então não faz sentido
embrulhar um `Sensor` em volta de um slot de `StateRegistry` só pra publicar isso. Em vez disso,
`RuntimeControl` implementa `Sensor` diretamente (já é `Send + Sync`, já tem `t_h()`) — o adaptador
OPC-UA (`adapter/opcua.rs`) trata `clock.t_h` como só mais um node de leitura, sem mecanismo
dedicado.
*/
impl crate::sensor::Sensor for RuntimeControl {
    fn read(&self) -> f64 {
        self.t_h()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_running_at_speed_1_and_t_h_zero() {
        let control = RuntimeControl::new();
        assert!(!control.is_paused());
        assert_eq!(control.speed(), 1.0);
        assert_eq!(control.t_h(), 0.0);
    }

    #[test]
    fn pause_and_resume_toggle_is_paused() {
        let control = RuntimeControl::new();
        control.pause();
        assert!(control.is_paused());
        control.resume();
        assert!(!control.is_paused());
    }

    #[test]
    fn negative_speed_clamps_to_zero() {
        let control = RuntimeControl::new();
        control.set_speed(-5.0);
        assert_eq!(control.speed(), 0.0);
    }

    #[test]
    fn advance_t_h_accumulates() {
        let control = RuntimeControl::new();
        control.advance_t_h(0.1);
        control.advance_t_h(0.2);
        assert!((control.t_h() - 0.3).abs() < 1e-12);
    }

    #[test]
    fn reset_request_is_consumed_exactly_once() {
        let control = RuntimeControl::new();
        control.request_reset();
        assert!(control.take_reset_request());
        assert!(!control.take_reset_request());
    }
}
