// core/src/numerical_method/integrator.rs

/** Contrato entre o `Integrator` e quem orquestra a simulação (`Simulation`,
seção 9 do plano). O `Integrator` não sabe nada de `Proxy`/`StateRegistry`/
`DynamicModel` — só soma vetores (seção 1.1.1: "o RK4 nunca assume nada
sobre o modelo além desse contrato").

`dynamics` é a closure que `Simulation` fornece (seção 9.6): recebe um
estado perturbado (um k-ésimo sub-passo) e devolve o vetor de derivadas
correspondente — por trás, ela escreve o estado perturbado nos `Proxy`s
certos e dispara `evaluate()` da árvore inteira de `DynamicModel`s, mas o
`Integrator` não sabe disso, só vê `&[f64] -> Vec<f64>`.

`step()` devolve o novo estado (não muta `state` — quem chama decide o que
fazer com o resultado, ex.: escrever de volta nos `Proxy`s antes de
commitar).
*/
pub trait Integrator {
    fn name(&self) -> &'static str {
        "unnamed"
    }

    fn step(
        &self,
        state: &[f64],
        dt: f64,
        dynamics: &mut dyn FnMut(&[f64]) -> Vec<f64>,
    ) -> Vec<f64>;
}
