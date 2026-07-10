// core/src/numerical_method/rk4.rs

use crate::numerical_method::integrator::Integrator;

/** Runge-Kutta de 4ª ordem clássico. Não sabe nada de `Proxy`/`DynamicModel`
— só recebe `state` (o vetor de estado no início do passo) e `dynamics`
(seção 9.6 do plano: a closure que `Simulation` monta, que escreve o estado
perturbado nos `Proxy`s certos, dispara `evaluate()` da árvore inteira, e lê
de volta as derivadas resultantes).
*/
pub struct RK4;

impl Integrator for RK4 {
    fn name(&self) -> &'static str {
        "RK4"
    }

    fn step(&self, state: &[f64], dt: f64, dynamics: &mut dyn FnMut(&[f64]) -> Vec<f64>) -> Vec<f64> {
        let n = state.len();

        // k1: derivada no início do passo, avaliada no estado atual (t).
        let k1 = dynamics(state);

        // s2: estado estimado em t + dt/2, avançando meio passo com k1.
        // k2: derivada nesse ponto médio (primeira estimativa do meio do passo).
        let s2: Vec<f64> = (0..n).map(|i| state[i] + 0.5 * dt * k1[i]).collect();
        let k2 = dynamics(&s2);

        // s3: estado estimado em t + dt/2 de novo, mas agora avançando com k2
        // (refina a estimativa do ponto médio usando a derivada anterior).
        // k3: derivada nesse ponto médio refinado.
        let s3: Vec<f64> = (0..n).map(|i| state[i] + 0.5 * dt * k2[i]).collect();
        let k3 = dynamics(&s3);

        // s4: estado estimado no fim do passo (t + dt), avançando um passo cheio com k3.
        // k4: derivada no fim do passo.
        let s4: Vec<f64> = (0..n).map(|i| state[i] + dt * k3[i]).collect();
        let k4 = dynamics(&s4);

        // Combina as 4 derivadas em média ponderada (Simpson): início e fim
        // têm peso 1, os dois pontos médios têm peso 2 (contam em dobro).
        (0..n)
            .map(|i| state[i] + dt / 6.0 * (k1[i] + 2.0 * k2[i] + 2.0 * k3[i] + k4[i]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dy/dt = y, y(0) = 1 → y(t) = e^t. RK4 não é exato, mas com passo
    /// pequeno deve chegar bem perto do valor analítico.
    #[test]
    fn integrates_exponential_growth_close_to_analytical_solution() {
        let rk4 = RK4;
        let mut state = vec![1.0];
        let dt = 0.01;

        for _ in 0..100 {
            state = rk4.step(&state, dt, &mut |s: &[f64]| vec![s[0]]);
        }

        let expected = std::f64::consts::E;
        assert!(
            (state[0] - expected).abs() < 1e-6,
            "esperava ~{expected}, obteve {}",
            state[0]
        );
    }

    /// dynamics() precisa ser chamada exatamente 4 vezes por step — uma por
    /// sub-passo (k1..k4), nem mais nem menos.
    #[test]
    fn calls_dynamics_exactly_four_times_per_step() {
        let rk4 = RK4;
        let mut calls = 0;
        rk4.step(&[0.0], 1.0, &mut |s: &[f64]| {
            calls += 1;
            vec![s[0]]
        });
        assert_eq!(calls, 4);
    }
}
