// core/src/method/rk4.rs

// use crate::dynamic_model::DynamicModel;
// use crate::method::integrator::Integrator;

pub struct RK4;

// impl Integrator for RK4 {
//
//     fn name(&self) -> &'static str { "RK4" }
//
//     fn step(&self, model: &mut dyn DynamicModel, state: &mut Vec<f64>, dt: f64) {
//
//         let n = state.len();
//
//         // k1: derivada no início do passo, avaliada no estado atual (t).
//         let k1 = model.dynamics(state);
//
//         // s2: estado estimado em t + dt/2, avançando meio passo com k1.
//         // k2: derivada nesse ponto médio (primeira estimativa do meio do passo).
//         let s2: Vec<f64> = (0..n).map(|i| state[i] + 0.5 * dt * k1[i]).collect();
//         let k2 = model.dynamics(&s2);
//
//         // s3: estado estimado em t + dt/2 de novo, mas agora avançando com k2
//         // (refina a estimativa do ponto médio usando a derivada anterior).
//         // k3: derivada nesse ponto médio refinado.
//         let s3: Vec<f64> = (0..n).map(|i| state[i] + 0.5 * dt * k2[i]).collect();
//         let k3 = model.dynamics(&s3);
//
//         // s4: estado estimado no fim do passo (t + dt), avançando um passo cheio com k3.
//         // k4: derivada no fim do passo.
//         let s4: Vec<f64> = (0..n).map(|i| state[i] + dt * k3[i]).collect();
//         let k4 = model.dynamics(&s4);
//
//         // Combina as 4 derivadas em média ponderada (Simpson) e integra o passo:
//         // início e fim têm peso 1, os dois pontos médios têm peso 2 (contam em dobro).
//         for i in 0..n {
//             state[i] += dt / 6.0 * (k1[i] + 2.0 * k2[i] + 2.0 * k3[i] + k4[i]);
//         }
//     }
//
// }
