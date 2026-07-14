pub mod integrator;
pub mod interator;
pub mod rk4;

use integrator::Integrator;
use rk4::RK4;

/** Os métodos numéricos que `Simulation::set_numerical_method()` aceita —
um enum fechado, não um `Box<dyn Integrator>` aberto de fora: só tolera o
que o framework já implementa aqui dentro (hoje só `RK4`), nunca uma
implementação arbitrária de quem consome o framework.
*/
#[derive(Debug, Clone, Copy)]
pub enum NumericalMethod {
    RK4,
}

impl NumericalMethod {
    pub(crate) fn integrator(&self) -> Box<dyn Integrator> {
        match self {
            NumericalMethod::RK4 => Box::new(RK4),
        }
    }
}

impl Default for NumericalMethod {
    fn default() -> Self {
        NumericalMethod::RK4
    }
}
