pub mod integrator;
pub mod interator;
pub mod rk4;

use integrator::Integrator;
use rk4::RK4;

/** Os métodos numéricos que `Simulation::set_numerical_method()` aceita — um enum fechado, não um
`Box<dyn Integrator>` aberto de fora: só tolera o que o framework já implementa aqui dentro (hoje só
`RK4`), nunca uma implementação arbitrária de quem consome o framework.
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

/** Permite escolher o método numérico por configuração (`[monjolo] numerical_method = "rk4"` em
`application.toml`, ver `crate::runtime::Runtime::bootstrap()`) em vez de só por código Rust — a
mesma ideia de `application.properties` escolhendo uma estratégia no Spring, sem uma classe/função
decidindo isso à mão. Comparação case-insensitive (`"RK4"`/`"rk4"`/`"Rk4"` tratados igual) — não é
uma chave de `StateRegistry` nem nada sensível a maiúscula/minúscula por convenção do resto do
framework, só uma string de configuração escrita à mão por humano.
*/
impl std::str::FromStr for NumericalMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "rk4" => Ok(NumericalMethod::RK4),
            other => Err(format!(
                "método numérico desconhecido: '{other}' (esperava \"rk4\")"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_accepts_rk4_case_insensitively() {
        assert!(matches!("rk4".parse::<NumericalMethod>(), Ok(NumericalMethod::RK4)));
        assert!(matches!("RK4".parse::<NumericalMethod>(), Ok(NumericalMethod::RK4)));
        assert!(matches!("Rk4".parse::<NumericalMethod>(), Ok(NumericalMethod::RK4)));
    }

    #[test]
    fn from_str_rejects_unknown_values() {
        assert!("euler".parse::<NumericalMethod>().is_err());
    }
}
