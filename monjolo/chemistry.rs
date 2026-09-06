/** monjolo/chemistry.rs

Correlações termodinâmicas de mistura genéricas — entalpia, temperatura a partir de entalpia
(Newton-Raphson), densidade líquida. Não sabem nada de TEP nem de nenhuma planta específica: dado
um conjunto de N componentes e os coeficientes empíricos de cada correlação (Antoine, entalpia
líquida/vapor, densidade), calculam a mesma física de mistura que qualquer simulador de processo
químico precisaria — só que aqui, genérico sobre N via const generic, em vez de fixo em 8
componentes.

Origem: tradução direta de SUBROUTINE TESUB1-4 do FORTRAN original do Tennessee Eastman Process
(teprob.f) — mas a MATEMÁTICA em si (leis de mistura ponderadas por fração molar, Antoine, Newton-
Raphson) não é específica do TEP; só os NÚMEROS (pesos moleculares, coeficientes de Antoine dos 8
componentes A-H) são. Por isso este módulo mora aqui, atrás da feature opcional `chemistry` (mesmo
padrão de `opcua` — ver `Cargo.toml`), e os números ficam do lado de quem monta a planta (ex.:
`tep-plant/src/physics/constants.rs`).
*/

/** Coeficientes empíricos de N componentes, um conjunto por correlação — equivalente ao bloco
COMMON /CONST/ do FORTRAN, generalizado por tamanho. Indexação de componente é responsabilidade de
quem constrói (ex.: A=0, B=1, ... no TEP) — este tipo não atribui significado a nenhum índice.
*/
pub struct Coefficients<const N: usize> {
    /** Massas molares [g/mol]. */
    pub xmw: [f64; N],

    /** Coeficientes da equação de Antoine para pressão de vapor. ln(P_vap) = AVP + BVP / (T + CVP).
    Componentes sem equação de Antoine (gases ideais, no TEP) usam coeficiente 0.
    */
    pub avp: [f64; N],
    pub bvp: [f64; N],
    pub cvp: [f64; N],

    /** Coeficientes de densidade líquida empírica. ρ = 1 / Σ( x_i * XMW_i / (AD_i + (BD_i +
    CD_i*T)*T) )
    */
    pub ad: [f64; N],
    pub bd: [f64; N],
    pub cd: [f64; N],

    /** Coeficientes de entalpia para fase líquida. H_i(T) = T * (AH + BH*T/2 + CH*T²/3) * 1.8 */
    pub ah: [f64; N],
    pub bh: [f64; N],
    pub ch: [f64; N],

    /** Coeficientes de entalpia para fase vapor. H_i(T) = T * (AG + BG*T/2 + CG*T²/3) * 1.8 + AV */
    pub ag: [f64; N],
    pub bg: [f64; N],
    pub cg: [f64; N],

    /** Entalpia de vaporização [cal/mol] (offset para fase vapor). */
    pub av: [f64; N],
}

/** Compute mixture enthalpy. Given molar fractions `z[N]` and temperature `t` (°C), returns the
total enthalpy of the mixture. `ity` controls the phase model:
  0 = liquid
  1 = vapor
  2 = vapor with ideal gas correction (PV)
*/
pub fn mixture_enthalpy<const N: usize>(z: &[f64; N], t: f64, ity: i32, constants: &Coefficients<N>) -> f64 {
    let mut h = 0.0_f64;

    if ity == 0 {
        /* Liquid: HI = T*(AH + BH*T/2 + CH*T²/3) * 1.8 */
        for i in 0..N {
            let hi = t * (constants.ah[i] + constants.bh[i] * t / 2.0 + constants.ch[i] * t * t / 3.0);
            h += z[i] * constants.xmw[i] * 1.8 * hi;
        }
    } else {
        /* Vapor: HI = T*(AG + BG*T/2 + CG*T²/3) * 1.8 + AV */
        for i in 0..N {
            let hi = t * (constants.ag[i] + constants.bg[i] * t / 2.0 + constants.cg[i] * t * t / 3.0);
            h += z[i] * constants.xmw[i] * (1.8 * hi + constants.av[i]);
        }
    }

    /* Ideal gas correction (ity == 2): H -= R*(T + 273.15) */
    if ity == 2 {
        h -= 3.57696e-6 * (t + 273.15);
    }

    h
}

/** Compute temperature from enthalpy via Newton-Raphson. Solves T such that `mixture_enthalpy(z, T,
ity) == h_target`. Starts from `t_init` and iterates until |ΔT| < 1e-12 or 100 iterations are
exhausted (returns `t_init` on failure).
*/
pub fn temperature_from_enthalpy<const N: usize>(
    z: &[f64; N],
    t_init: f64,
    h_target: f64,
    ity: i32,
    constants: &Coefficients<N>,
) -> f64 {
    let mut t = t_init;

    for _ in 0..100 {
        let h_test = mixture_enthalpy(z, t, ity, constants);
        let dh = enthalpy_derivative(z, t, ity, constants);
        let dt = -(h_test - h_target) / dh;
        t += dt;
        if dt.abs() < 1.0e-12 {
            return t;
        }
    }

    t_init
}

/** Compute dH/dT (enthalpy derivative with respect to temperature). Used as the Jacobian in the
Newton-Raphson loop of `temperature_from_enthalpy`.
*/
pub fn enthalpy_derivative<const N: usize>(z: &[f64; N], t: f64, ity: i32, constants: &Coefficients<N>) -> f64 {
    let mut dh = 0.0_f64;

    if ity == 0 {
        for i in 0..N {
            let dhi = (constants.ah[i] + constants.bh[i] * t + constants.ch[i] * t * t) * 1.8;
            dh += z[i] * constants.xmw[i] * dhi;
        }
    } else {
        for i in 0..N {
            let dhi = (constants.ag[i] + constants.bg[i] * t + constants.cg[i] * t * t) * 1.8;
            dh += z[i] * constants.xmw[i] * dhi;
        }
    }

    if ity == 2 {
        dh -= 3.57696e-6;
    }

    dh
}

/** Compute liquid mixture density. Empirical correlation: V = Σ( x_i * XMW_i / (AD_i + (BD_i +
CD_i*T)*T) ), ρ = 1 / V.
*/
pub fn liquid_density<const N: usize>(x: &[f64; N], t: f64, constants: &Coefficients<N>) -> f64 {
    let v: f64 = (0..N)
        .map(|i| x[i] * constants.xmw[i] / (constants.ad[i] + (constants.bd[i] + constants.cd[i] * t) * t))
        .sum();
    1.0 / v
}

#[cfg(test)]
mod tests {
    use super::*;

    /* Coeficientes de UM componente só, escolhidos pra fazer a mão: AH=1.0e-6 (resto zero) dá
    H_liquid(T) = T² * 1.8e-6 * XMW pra fração molar 1.0 — inversível em forma fechada, útil pra
    provar que temperature_from_enthalpy converge pro valor certo sem depender de dados reais do
    TEP (que moram em tep-plant, não aqui).
    */
    fn single_component_coefficients() -> Coefficients<1> {
        Coefficients {
            xmw: [1.0],
            avp: [0.0],
            bvp: [0.0],
            cvp: [0.0],
            ad: [1.0],
            bd: [0.0],
            cd: [0.0],
            ah: [1.0e-6],
            bh: [0.0],
            ch: [0.0],
            ag: [0.0],
            bg: [0.0],
            cg: [0.0],
            av: [0.0],
        }
    }

    #[test]
    fn mixture_enthalpy_matches_hand_computed_value_for_pure_liquid() {
        let constants = single_component_coefficients();
        let z = [1.0];
        // H = T * (AH) * 1.8 * XMW = T * 1.0e-6 * 1.8 * 1.0, pra T=100: 1.8e-4
        assert_eq!(mixture_enthalpy(&z, 100.0, 0, &constants), 100.0 * 1.0e-6 * 1.8);
    }

    #[test]
    fn temperature_from_enthalpy_inverts_mixture_enthalpy() {
        let constants = single_component_coefficients();
        let z = [1.0];
        let target_temperature = 87.3;
        let target_enthalpy = mixture_enthalpy(&z, target_temperature, 0, &constants);

        let recovered = temperature_from_enthalpy(&z, 50.0, target_enthalpy, 0, &constants);

        assert!(
            (recovered - target_temperature).abs() < 1e-6,
            "esperava recuperar {target_temperature}, obteve {recovered}",
        );
    }

    #[test]
    fn liquid_density_matches_hand_computed_value() {
        let constants = single_component_coefficients();
        let x = [1.0];
        // V = 1.0 * 1.0 / (1.0 + 0.0) = 1.0 -> densidade = 1.0
        assert_eq!(liquid_density(&x, 42.0, &constants), 1.0);
    }
}
