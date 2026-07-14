// core/src/numerical_method/interator.rs

/** Contraparte de `Integrator` (ver `integrator.rs`) para a dimensão
algébrica, não a temporal. `Integrator` resolve "como o estado avança no
tempo" (`dt`); `Interator` resolveria "como um ciclo algébrico entre
componentes converge dentro de uma mesma rodada de avaliação" — quando o
grafo de dependências não é um DAG (README, "Limite estrutural: DAG, não
DAE"): dois (ou mais) componentes que precisam, simultaneamente, do valor
um do outro no mesmo instante.

Vazio de propósito — ainda não decidimos a assinatura real (candidato mais
provável: Newton-Raphson sobre o resíduo do ciclo, análogo a como `RK4`
implementa `Integrator`, mas isso ainda não foi discutido/fechado). Existe
aqui só para marcar o lugar do conceito, e para o crate falar dele por
nome — não por uma ideia solta em comentário.
*/
pub trait Interator {
    fn name(&self) -> &'static str {
        "unnamed"
    }
}
