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

/** Fase física de uma `Mixture` — vapor, líquido, ou `Mixed` (combinação de mais de uma fase, ex.:
o inventário TOTAL de um vaso que tem as duas ao mesmo tempo). `Mixture::add` degrada pra `Mixed`
automaticamente quando soma duas fases diferentes; somar a mesma fase preserva a fase.
*/
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Vapor,
    Liquid,
    Mixed,
}

/** Catálogo de nomes de componente ("A", "B", ...), compartilhado por referência `&'static` — várias
`Mixture` do mesmo processo apontam pro MESMO catálogo, sem duplicar as strings por instância.
Definido por quem monta a planta (ex.: `tep-plant`), não por este módulo — nomear componentes é
identidade da planta, não da matemática de mistura.
*/
pub type Species<const N: usize> = [&'static str; N];

/** Composição/inventário de N espécies químicas — o `[f64; N]` que descreve uma corrente, um vaso,
um estado, deixa de ser um array cru repetido em cada unidade e vira um tipo com identidade própria:
sabe seu total, sua fase, o nome de cada componente, e as operações que já se repetiam por toda
parte (soma, normalização, produto escalar, pressão parcial) como método nomeado, não `for i in
0..N` reescrito a cada chamada.

Filosofia: a maioria das `Mixture` do sistema nasce de OUTRA `Mixture` já existente — soma (`+`),
subtração (`-`), escala (`scaled_by`), normalização (`mole_fractions`) — nunca de um array digitado
à mão. `Mixture::new`/`Mixture::zero` existem pra fronteira (ex.: o feed externo de uma planta,
que introduz massa nova vinda de fora do sistema) ou pra teste — código que só COMBINA misturas que
já existem deveria raramente precisar delas.
*/
#[derive(Clone, Copy, Debug)]
pub struct Mixture<const N: usize> {
    values: [f64; N],
    phase: Phase,
    species: &'static Species<N>,
}

impl<const N: usize> Mixture<N> {
    /** Todas as N posições em zero — pra quando o valor inicial é "nada aqui ainda", sem o chamador
    precisar escrever `[0.0; N]` toda vez.
    */
    pub fn zero(phase: Phase, species: &'static Species<N>) -> Self {
        Self { values: [0.0; N], phase, species }
    }

    pub fn new(values: [f64; N], phase: Phase, species: &'static Species<N>) -> Self {
        Self { values, phase, species }
    }

    /** Constrói a partir de um array MENOR (M componentes), colocado a partir de `offset`, com o
    resto das N posições em zero — pra quando só parte dos componentes faz sentido aqui (ex.: o
    vapor do reator só tem A/B/C, mora nas 3 primeiras posições de um total de 8; sem isto, quem
    chama precisaria escrever `std::array::from_fn` + `if` toda vez que isso acontece).
    */
    pub fn at<const M: usize>(offset: usize, values: &[f64; M], phase: Phase, species: &'static Species<N>) -> Self {
        let mut full = [0.0; N];
        full[offset..offset + M].copy_from_slice(values);
        Self { values: full, phase, species }
    }

    pub fn component(&self, i: usize) -> f64 {
        self.values[i]
    }

    pub fn name(&self, i: usize) -> &'static str {
        self.species[i]
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn as_array(&self) -> [f64; N] {
        self.values
    }

    pub fn total(&self) -> f64 {
        self.values.iter().sum()
    }

    /** Moles (ou o que quer que `self` represente) → fração do próprio total. */
    pub fn mole_fractions(&self) -> Self {
        let total = self.total();
        Self { values: std::array::from_fn(|i| self.values[i] / total), phase: self.phase, species: self.species }
    }

    pub fn scaled_by(&self, factor: f64) -> Self {
        Self { values: std::array::from_fn(|i| self.values[i] * factor), phase: self.phase, species: self.species }
    }

    /** Produto escalar componente-a-componente com um array externo (ex.: composição · peso
    molecular = peso molecular médio da mistura).
    */
    pub fn dot(&self, other: &[f64; N]) -> f64 {
        (0..N).map(|i| self.values[i] * other[i]).sum()
    }

    /** Quanto o total DESTA mistura representa do total de OUTRA que a contém — ex.: a fase vapor
    de um vaso sobre o inventário total do mesmo vaso.
    */
    pub fn fraction_of(&self, whole: &Self) -> f64 {
        self.total() / whole.total()
    }

    /** Pressão parcial de gás ideal (P_i = n_i·R·T/V) — só fisicamente correto pra componentes que
    `self` representa como moles de vapor genuíno (não como fração molar de uma fase líquida).
    */
    pub fn ideal_gas_pressure(&self, temperature_k: f64, volume: f64, gas_constant: f64) -> Self {
        Self {
            values: std::array::from_fn(|i| self.values[i] * gas_constant * temperature_k / volume),
            phase: self.phase,
            species: self.species,
        }
    }

    /** Pressão de vapor tipo Antoine (exp(AVP + BVP/(T+CVP))) × fração molar — a contribuição de
    componentes semi-voláteis de uma fase líquida em equilíbrio líquido-vapor. Componentes sem
    equação de Antoine (avp=bvp=cvp=0) dão `exp(0) * fração = fração`, não zero — quem chama
    continua responsável por saber quais índices fazem sentido fisicamente aqui.
    */
    pub fn vapor_pressure(&self, temperature: f64, constants: &Coefficients<N>) -> Self {
        Self {
            values: std::array::from_fn(|i| {
                (constants.avp[i] + constants.bvp[i] / (temperature + constants.cvp[i])).exp() * self.values[i]
            }),
            phase: self.phase,
            species: self.species,
        }
    }

    /** Atalho pra `mixture_enthalpy(&self.as_array(), ...)` — mesma correlação, só chamada como
    método em vez de função livre com o array desembrulhado à mão.
    */
    pub fn enthalpy(&self, temperature: f64, ity: i32, constants: &Coefficients<N>) -> f64 {
        mixture_enthalpy(&self.values, temperature, ity, constants)
    }
}

/** Soma componente-a-componente. Fase resultante: preserva a fase quando as duas são iguais,
degrada pra `Phase::Mixed` quando somam fases diferentes (ex.: vapor + líquido = inventário total
de um vaso, que genuinamente tem as duas fases ao mesmo tempo).
*/
impl<const N: usize> std::ops::Add for Mixture<N> {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        let phase = if self.phase == other.phase { self.phase } else { Phase::Mixed };
        Self { values: std::array::from_fn(|i| self.values[i] + other.values[i]), phase, species: self.species }
    }
}

/** Subtração componente-a-componente — preserva a fase de `self` (ex.: "o que sai desta fase",
não uma combinação de fases diferentes).
*/
impl<const N: usize> std::ops::Sub for Mixture<N> {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self { values: std::array::from_fn(|i| self.values[i] - other.values[i]), phase: self.phase, species: self.species }
    }
}

/** Rede de reações de uma planta — estequiometria e calor de cada uma de R reações sobre N espécies.
Dado da PLANTA (os números), mesmo padrão de `Coefficients<N>`: este módulo só conhece a forma, quem
monta a planta (ex.: `tep-plant`) fornece os valores.
*/
pub struct ReactionScheme<const N: usize, const R: usize> {
    /** `stoichiometry[r][i]`: kmol da espécie `i` produzidos (+) ou consumidos (−) por kmol de avanço
    da reação `r`.
    */
    pub stoichiometry: [[f64; N]; R],

    /** Calor liberado por kmol de avanço da reação `r`. */
    pub enthalpies: [f64; R],
}

/** Taxa de avanço de cada uma das R reações de um `ReactionScheme<N, R>` — o que a cinética calcula
(taxas brutas por reação) e o que o resto do sistema realmente consome (consumo/produção líquida por
espécie e calor total), sem cada unidade reimplementar a estequiometria com `for`/índice à mão.

Diferente de `Mixture`, não tem fase: uma taxa é uma propriedade da reação, não de um vapor ou
líquido.
*/
#[derive(Clone, Copy)]
pub struct Reaction<const N: usize, const R: usize> {
    rates: [f64; R],
    scheme: &'static ReactionScheme<N, R>,
}

impl<const N: usize, const R: usize> Reaction<N, R> {
    pub fn new(rates: [f64; R], scheme: &'static ReactionScheme<N, R>) -> Self {
        Self { rates, scheme }
    }

    pub fn rate(&self, r: usize) -> f64 {
        self.rates[r]
    }

    /** Consumo (−) ou produção (+) líquida de cada espécie, somando a contribuição de todas as
    reações.
    */
    pub fn species_rates(&self) -> [f64; N] {
        std::array::from_fn(|i| (0..R).map(|r| self.scheme.stoichiometry[r][i] * self.rates[r]).sum())
    }

    /** Calor total liberado por todas as reações. */
    pub fn heat(&self) -> f64 {
        (0..R).map(|r| self.scheme.enthalpies[r] * self.rates[r]).sum()
    }
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

    const TEST_SPECIES: Species<3> = ["X", "Y", "Z"];

    #[test]
    fn zero_is_all_zero_without_the_caller_writing_it() {
        let m = Mixture::zero(Phase::Vapor, &TEST_SPECIES);
        assert_eq!(m.as_array(), [0.0, 0.0, 0.0]);
        assert_eq!(m.phase(), Phase::Vapor);
        assert_eq!(m.name(1), "Y");
    }

    #[test]
    fn at_places_a_smaller_array_at_an_offset_and_zero_pads_the_rest() {
        let m: Mixture<5> = Mixture::at(2, &[7.0, 8.0], Phase::Liquid, &["a", "b", "c", "d", "e"]);
        assert_eq!(m.as_array(), [0.0, 0.0, 7.0, 8.0, 0.0]);
    }

    #[test]
    fn total_sums_every_component() {
        let m = Mixture::new([1.0, 2.0, 3.0], Phase::Liquid, &TEST_SPECIES);
        assert_eq!(m.total(), 6.0);
    }

    #[test]
    fn mole_fractions_normalizes_by_the_own_total_and_keeps_the_phase() {
        let m = Mixture::new([1.0, 1.0, 2.0], Phase::Liquid, &TEST_SPECIES);
        let fractions = m.mole_fractions();
        assert_eq!(fractions.as_array(), [0.25, 0.25, 0.5]);
        assert_eq!(fractions.phase(), Phase::Liquid);
    }

    #[test]
    fn scaled_by_multiplies_every_component() {
        let m = Mixture::new([1.0, 2.0, 3.0], Phase::Vapor, &TEST_SPECIES);
        assert_eq!(m.scaled_by(10.0).as_array(), [10.0, 20.0, 30.0]);
    }

    #[test]
    fn dot_is_the_componentwise_scalar_product() {
        let m = Mixture::new([1.0, 2.0, 3.0], Phase::Vapor, &TEST_SPECIES);
        assert_eq!(m.dot(&[10.0, 10.0, 10.0]), 60.0);
    }

    #[test]
    fn fraction_of_divides_totals() {
        let part = Mixture::new([1.0, 1.0, 0.0], Phase::Vapor, &TEST_SPECIES);
        let whole = Mixture::new([2.0, 2.0, 4.0], Phase::Mixed, &TEST_SPECIES);
        assert_eq!(part.fraction_of(&whole), 2.0 / 8.0);
    }

    #[test]
    fn add_preserves_phase_when_both_sides_match_but_degrades_to_mixed_otherwise() {
        let vapor_a = Mixture::new([1.0, 0.0, 0.0], Phase::Vapor, &TEST_SPECIES);
        let vapor_b = Mixture::new([0.0, 1.0, 0.0], Phase::Vapor, &TEST_SPECIES);
        assert_eq!((vapor_a + vapor_b).phase(), Phase::Vapor);
        assert_eq!((vapor_a + vapor_b).as_array(), [1.0, 1.0, 0.0]);

        let liquid = Mixture::new([0.0, 0.0, 1.0], Phase::Liquid, &TEST_SPECIES);
        assert_eq!((vapor_a + liquid).phase(), Phase::Mixed);
        assert_eq!((vapor_a + liquid).as_array(), [1.0, 0.0, 1.0]);
    }

    /** Prova que `Add` sozinho já cobre o caso que motivou um `splice`/merge por faixa de índice
    num rascunho anterior (ver tep-plant/docs/streams-spring-style): duas Mixture, cada uma válida
    só numa faixa de índices e ZERO na outra (por construção — `zero()` + preencher só a parte que
    faz sentido), somadas dão exatamente a combinação esperada, sem precisar de um método à parte.
    */
    #[test]
    fn add_combines_two_zero_padded_range_specific_mixtures_without_needing_a_splice_method() {
        let gas_partial = Mixture::new([5.0, 0.0, 0.0], Phase::Vapor, &TEST_SPECIES); // válido só no índice 0
        let liquid_partial = Mixture::new([0.0, 0.0, 9.0], Phase::Liquid, &TEST_SPECIES); // válido só no índice 2
        assert_eq!((gas_partial + liquid_partial).as_array(), [5.0, 0.0, 9.0]);
    }

    #[test]
    fn sub_preserves_the_phase_of_self() {
        let inflow = Mixture::new([5.0, 5.0, 5.0], Phase::Vapor, &TEST_SPECIES);
        let outflow = Mixture::new([1.0, 2.0, 3.0], Phase::Liquid, &TEST_SPECIES);
        let result = inflow - outflow;
        assert_eq!(result.as_array(), [4.0, 3.0, 2.0]);
        assert_eq!(result.phase(), Phase::Vapor);
    }

    #[test]
    fn ideal_gas_pressure_matches_pv_nrt() {
        let m = Mixture::new([2.0, 0.0, 0.0], Phase::Vapor, &TEST_SPECIES);
        // P = n*R*T/V = 2.0 * 10.0 * 300.0 / 100.0 = 60.0
        assert_eq!(m.ideal_gas_pressure(300.0, 100.0, 10.0).component(0), 60.0);
    }

    #[test]
    fn vapor_pressure_matches_antoine_times_mole_fraction() {
        const SPECIES: Species<1> = ["X"];
        let constants = single_component_coefficients();
        let m = Mixture::new([0.5], Phase::Liquid, &SPECIES);
        // avp=bvp=cvp=0 -> exp(0) = 1.0 -> resultado = 1.0 * fração = 0.5
        assert_eq!(m.vapor_pressure(42.0, &constants).component(0), 0.5);
    }

    #[test]
    fn mixture_enthalpy_method_matches_the_free_function() {
        const SPECIES: Species<1> = ["X"];
        let constants = single_component_coefficients();
        let m = Mixture::new([1.0], Phase::Liquid, &SPECIES);
        assert_eq!(m.enthalpy(100.0, 0, &constants), mixture_enthalpy(&[1.0], 100.0, 0, &constants));
    }

    /* Rede de duas reações sobre 3 espécies (X, Y, Z): X + Y → Z (calor 5.0) e 2 Z → 3 X (calor 0.0) */
    const TEST_SCHEME: ReactionScheme<3, 2> =
        ReactionScheme { stoichiometry: [[-1.0, -1.0, 1.0], [3.0, 0.0, -2.0]], enthalpies: [5.0, 0.0] };

    #[test]
    fn reaction_species_rates_sum_the_stoichiometry_of_every_reaction() {
        let reaction = Reaction::new([2.0, 1.0], &TEST_SCHEME);
        // X: -1*2 + 3*1 = 1.0 | Y: -1*2 + 0*1 = -2.0 | Z: 1*2 + -2*1 = 0.0
        assert_eq!(reaction.species_rates(), [1.0, -2.0, 0.0]);
    }

    #[test]
    fn reaction_heat_sums_enthalpy_times_rate_of_every_reaction() {
        let reaction = Reaction::new([2.0, 1.0], &TEST_SCHEME);
        assert_eq!(reaction.heat(), 10.0);
    }

    #[test]
    fn reaction_with_all_rates_at_zero_consumes_and_releases_nothing() {
        let reaction = Reaction::new([0.0, 0.0], &TEST_SCHEME);
        assert_eq!(reaction.species_rates(), [0.0, 0.0, 0.0]);
        assert_eq!(reaction.heat(), 0.0);
        assert_eq!(reaction.rate(1), 0.0);
    }
}
