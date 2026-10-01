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

/** Constante universal dos gases, R, nas unidades internas do TEP (`RG` em `teprob.f`): pressão em
mmHg, volume em ft³, quantidade em lbmol e temperatura em K — 998,9 mmHg·ft³/(lbmol·K). Sai da
conta 10,7316 psia·ft³/(lbmol·°R) × 51,7149 mmHg/psia × 1,8 °R/K. Vive aqui, e não em cada unidade,
porque não é dado de nenhuma unidade: é a mesma constante em qualquer vaso da planta.
*/
pub const GAS_CONSTANT: f64 = 998.9;

/** Lei de Arrhenius: quão rápido uma reação anda numa dada temperatura, `exp(a − Ea / (R·T))`.
`a` é o termo constante (o logaritmo do fator de frequência), `activation_energy` é a energia de
ativação em cal/mol (quanto maior, mais a reação acelera ao esquentar) e `R` é 1,987 cal/(mol·K).
*/
pub fn arrhenius(a: f64, activation_energy: f64, temperature_k: f64) -> f64 {
    (a - activation_energy / 1.987 / temperature_k).exp()
}

/** Catálogo de nomes de componente ("A", "B", ...), compartilhado por referência `&'static` — várias
`Mixture` do mesmo processo apontam pro MESMO catálogo, sem duplicar as strings por instância.
Definido por quem monta a planta (ex.: `tep-plant`), não por este módulo — nomear componentes é
identidade da planta, não da matemática de mistura.
*/
pub type Species<const N: usize> = [&'static str; N];

/** Quanto existe de cada uma de N substâncias — o `[f64; N]` que descreve uma corrente, um vaso,
um estado, deixa de ser um array cru repetido em cada unidade e vira um tipo com identidade própria:
sabe seu total, o nome de cada componente, e as operações que já se repetiam por toda parte (soma,
normalização, produto escalar, pressão parcial) como método nomeado, não `for i in 0..N` reescrito
a cada chamada. Serve tanto pra quantidades (moles) quanto pra frações, taxas ou vazões por
componente: o tipo não impõe soma 1 nem diz se é vapor ou líquido — quem dá esse significado é o nome
da variável de quem usa.

Filosofia: a maioria das `Mixture` do sistema nasce de OUTRA `Mixture` já existente — soma (`+`),
subtração (`-`), escala (`scaled_by`), normalização (`mole_fractions`) — nunca de um array digitado
à mão. `Mixture::new`/`Mixture::zero` existem pra fronteira (ex.: o feed externo de uma planta,
que introduz massa nova vinda de fora do sistema) ou pra teste — código que só COMBINA misturas que
já existem deveria raramente precisar delas.
*/
#[derive(Clone, Copy, Debug)]
pub struct Mixture<const N: usize> {
    values: [f64; N],
    species: &'static Species<N>,
}

impl<const N: usize> Mixture<N> {
    /** Todas as N posições em zero — pra quando o valor inicial é "nada aqui ainda", sem o chamador
    precisar escrever `[0.0; N]` toda vez.
    */
    pub fn zero(species: &'static Species<N>) -> Self {
        Self { values: [0.0; N], species }
    }

    pub fn new(values: [f64; N], species: &'static Species<N>) -> Self {
        Self { values, species }
    }

    /** Constrói a partir de um array MENOR (M componentes), colocado a partir de `offset`, com o
    resto das N posições em zero — pra quando só parte dos componentes faz sentido aqui (ex.: o
    vapor do reator só tem A/B/C, mora nas 3 primeiras posições de um total de 8; sem isto, quem
    chama precisaria escrever `std::array::from_fn` + `if` toda vez que isso acontece).
    */
    pub fn at<const M: usize>(offset: usize, values: &[f64; M], species: &'static Species<N>) -> Self {
        let mut full = [0.0; N];
        full[offset..offset + M].copy_from_slice(values);
        Self { values: full, species }
    }

    pub fn component(&self, i: usize) -> f64 {
        self.values[i]
    }

    /** O valor de uma espécie pelo NOME (`mixture.get("A")`), em vez de pelo índice. Entra em
    pânico se o nome não está no catálogo — um erro de digitação, não um caso a tratar.
    */
    pub fn get(&self, name: &str) -> f64 {
        match self.species.iter().position(|candidate| *candidate == name) {
            Some(i) => self.values[i],
            None => panic!("espécie `{name}` não está no catálogo {:?}", self.species),
        }
    }

    pub fn name(&self, i: usize) -> &'static str {
        self.species[i]
    }

    pub fn as_array(&self) -> [f64; N] {
        self.values
    }

    pub fn total(&self) -> f64 {
        self.values.iter().sum()
    }

    /** Moles (ou o que quer que `self` represente) → fração do próprio total. */
    /** Moles (ou o que `self` representa) → fração do próprio total. Total zero devolve tudo zero
    (não NaN) — "nada aqui, então nada é fração de nada" é uma resposta válida, diferente de
    "divisão inválida".
    */
    pub fn mole_fractions(&self) -> Self {
        let total = self.total();
        if total == 0.0 {
            return Self::zero(self.species);
        }
        Self { values: std::array::from_fn(|i| self.values[i] / total), species: self.species }
    }

    pub fn scaled_by(&self, factor: f64) -> Self {
        Self { values: std::array::from_fn(|i| self.values[i] * factor), species: self.species }
    }

    /** Produto escalar componente-a-componente com um array externo (ex.: composição · peso
    molecular = peso molecular médio da mistura).
    */
    pub fn dot(&self, other: &[f64; N]) -> f64 {
        (0..N).map(|i| self.values[i] * other[i]).sum()
    }

    /** Quanto o total DESTA mistura representa do total de OUTRA que a contém — ex.: a parte vapor
    de um vaso sobre o inventário total do mesmo vaso.
    */
    pub fn fraction_of(&self, whole: &Self) -> f64 {
        self.total() / whole.total()
    }

    /** Pressão parcial de gás ideal (P_i = n_i·R·T/V, com `GAS_CONSTANT`) — só fisicamente correto
    pra componentes que `self` representa como moles de vapor genuíno (não como fração molar de
    uma fase líquida).
    */
    pub fn ideal_gas_pressure(&self, temperature_k: f64, volume: f64) -> Self {
        Self {
            values: std::array::from_fn(|i| self.values[i] * GAS_CONSTANT * temperature_k / volume),
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

/** Soma componente-a-componente. */
impl<const N: usize> std::ops::Add for Mixture<N> {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self { values: std::array::from_fn(|i| self.values[i] + other.values[i]), species: self.species }
    }
}

/** Subtração componente-a-componente. */
impl<const N: usize> std::ops::Sub for Mixture<N> {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self { values: std::array::from_fn(|i| self.values[i] - other.values[i]), species: self.species }
    }
}

/** Multiplicação componente-a-componente (produto de Hadamard) — cada posição multiplica só pela
mesma posição da outra `Mixture`, sem somar nada no final (diferente de `dot`, que soma tudo num
único número). Pra multiplicar TODAS as posições pelo MESMO fator, use `scaled_by`, não isto.
*/
impl<const N: usize> std::ops::Mul for Mixture<N> {
    type Output = Self;
    fn mul(self, other: Self) -> Self {
        Self { values: std::array::from_fn(|i| self.values[i] * other.values[i]), species: self.species }
    }
}

/** Uma reação dentro de `Reactions`: a receita (quanto de cada espécie é gasto ou produzido), o calor
que libera e a fórmula que diz quão rápido ela anda dadas a temperatura e as pressões parciais.
*/
struct ReactionDefinition<const N: usize> {
    stoichiometry: [f64; N],
    heat: f64,
    rate: Box<dyn Fn(f64, &Mixture<N>) -> f64 + Send + Sync>,
}

/** As reações químicas de uma planta, montadas uma a uma pelo NOME das espécies, sem tabela de
índices:

```ignore
let reactions = Reactions::new(&SPECIES)
    .add("A + C + D -> G", 0.0689, |temperature_k, p| arrhenius(31.6, 40000.0, temperature_k) * p.get("A") * p.get("C") * p.get("D"))
    .add("1.5 D -> F", 0.0, |temperature_k, p| ...);
```

Cada reação carrega a própria receita, o próprio calor e a própria fórmula de velocidade, então nada
mais precisa saber "a reação 3 é o índice 2". Depois de montada, `at()` responde, pra um estado, quanto
de cada substância está sendo consumido ou produzido e quanto calor está sendo liberado.
*/
pub struct Reactions<const N: usize> {
    species: &'static Species<N>,
    reactions: Vec<ReactionDefinition<N>>,
}

/** O efeito conjunto de todas as reações de um `Reactions` num dado estado: consumo (−) ou produção
(+) líquida de cada espécie, somando todas as reações, e o calor total liberado.
*/
pub struct ReactionOutcome<const N: usize> {
    pub species_rates: Mixture<N>,
    pub heat: f64,
}

impl<const N: usize> Reactions<N> {
    pub fn new(species: &'static Species<N>) -> Self {
        Self { species, reactions: Vec::new() }
    }

    /** Acrescenta uma reação. `equation` é a receita escrita com os nomes das espécies, ex.: `"A + C +
    D -> G"` ou `"1.5 D -> F"` (o número antes do nome é a quantidade; sem número, 1). `heat` é o
    calor liberado por unidade da reação. `rate` recebe a temperatura em K e as pressões parciais e
    devolve a velocidade POR UNIDADE DE VOLUME — `at()` multiplica pelo volume.
    */
    pub fn add(
        mut self,
        equation: &str,
        heat: f64,
        rate: impl Fn(f64, &Mixture<N>) -> f64 + Send + Sync + 'static,
    ) -> Self {
        let stoichiometry = self.parse(equation);
        self.reactions.push(ReactionDefinition { stoichiometry, heat, rate: Box::new(rate) });
        self
    }

    fn parse(&self, equation: &str) -> [f64; N] {
        let (reactants, products) = equation
            .split_once("->")
            .unwrap_or_else(|| panic!("reação `{equation}` sem `->` separando reagentes de produtos"));

        let mut stoichiometry = [0.0; N];
        for (side, sign) in [(reactants, -1.0), (products, 1.0)] {
            for term in side.split('+') {
                let term = term.trim();
                let (amount, name) = match term.split_once(char::is_whitespace) {
                    Some((first, rest)) if first.parse::<f64>().is_ok() => (first.parse::<f64>().unwrap(), rest.trim()),
                    _ => (1.0, term),
                };
                let index = self.species.iter().position(|candidate| *candidate == name).unwrap_or_else(|| {
                    panic!("espécie `{name}` da reação `{equation}` não está no catálogo {:?}", self.species)
                });
                stoichiometry[index] += sign * amount;
            }
        }
        stoichiometry
    }

    /** O efeito de todas as reações na temperatura `temperature_k` (K) e nas pressões parciais
    `partial_pressures`, num vaso com `volume` de gás.
    */
    pub fn at(&self, temperature_k: f64, partial_pressures: &Mixture<N>, volume: f64) -> ReactionOutcome<N> {
        let rates: Vec<f64> =
            self.reactions.iter().map(|reaction| (reaction.rate)(temperature_k, partial_pressures) * volume).collect();

        let species_rates = std::array::from_fn(|i| {
            self.reactions.iter().zip(&rates).map(|(reaction, rate)| reaction.stoichiometry[i] * rate).sum()
        });
        let heat = self.reactions.iter().zip(&rates).map(|(reaction, rate)| reaction.heat * rate).sum();

        ReactionOutcome { species_rates: Mixture::new(species_rates, self.species), heat }
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

    #[test]
    fn arrhenius_matches_the_closed_form() {
        // exp(2.0 - 1.987 / 1.987 / 1.0) = exp(1.0)
        assert_eq!(arrhenius(2.0, 1.987, 1.0), (2.0f64 - 1.987 / 1.987 / 1.0).exp());
    }

    const TEST_SPECIES: Species<3> = ["X", "Y", "Z"];

    #[test]
    fn zero_is_all_zero_without_the_caller_writing_it() {
        let m = Mixture::zero(&TEST_SPECIES);
        assert_eq!(m.as_array(), [0.0, 0.0, 0.0]);
        assert_eq!(m.name(1), "Y");
    }

    #[test]
    fn at_places_a_smaller_array_at_an_offset_and_zero_pads_the_rest() {
        let m: Mixture<5> = Mixture::at(2, &[7.0, 8.0], &["a", "b", "c", "d", "e"]);
        assert_eq!(m.as_array(), [0.0, 0.0, 7.0, 8.0, 0.0]);
    }

    #[test]
    fn get_reads_a_component_by_species_name() {
        let m = Mixture::new([1.0, 2.0, 3.0], &TEST_SPECIES);
        assert_eq!(m.get("Y"), 2.0);
    }

    #[test]
    #[should_panic(expected = "não está no catálogo")]
    fn get_panics_on_an_unknown_species_name() {
        Mixture::new([1.0, 2.0, 3.0], &TEST_SPECIES).get("Q");
    }

    #[test]
    fn total_sums_every_component() {
        let m = Mixture::new([1.0, 2.0, 3.0], &TEST_SPECIES);
        assert_eq!(m.total(), 6.0);
    }

    #[test]
    fn mole_fractions_normalizes_by_the_own_total() {
        let m = Mixture::new([1.0, 1.0, 2.0], &TEST_SPECIES);
        assert_eq!(m.mole_fractions().as_array(), [0.25, 0.25, 0.5]);
    }

    #[test]
    fn mole_fractions_of_a_zero_total_is_zero_not_nan() {
        let m = Mixture::zero(&TEST_SPECIES);
        assert_eq!(m.mole_fractions().as_array(), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn scaled_by_multiplies_every_component() {
        let m = Mixture::new([1.0, 2.0, 3.0], &TEST_SPECIES);
        assert_eq!(m.scaled_by(10.0).as_array(), [10.0, 20.0, 30.0]);
    }

    #[test]
    fn dot_is_the_componentwise_scalar_product() {
        let m = Mixture::new([1.0, 2.0, 3.0], &TEST_SPECIES);
        assert_eq!(m.dot(&[10.0, 10.0, 10.0]), 60.0);
    }

    #[test]
    fn fraction_of_divides_totals() {
        let part = Mixture::new([1.0, 1.0, 0.0], &TEST_SPECIES);
        let whole = Mixture::new([2.0, 2.0, 4.0], &TEST_SPECIES);
        assert_eq!(part.fraction_of(&whole), 2.0 / 8.0);
    }

    #[test]
    fn add_and_sub_work_component_by_component() {
        let a = Mixture::new([1.0, 0.0, 0.0], &TEST_SPECIES);
        let b = Mixture::new([0.0, 1.0, 0.0], &TEST_SPECIES);
        assert_eq!((a + b).as_array(), [1.0, 1.0, 0.0]);
        assert_eq!((a - b).as_array(), [1.0, -1.0, 0.0]);
    }

    #[test]
    fn mul_multiplies_component_by_the_same_component_of_the_other_mixture() {
        let a = Mixture::new([2.0, 3.0, 4.0], &TEST_SPECIES);
        let b = Mixture::new([5.0, 0.0, 1.0], &TEST_SPECIES);
        assert_eq!((a * b).as_array(), [10.0, 0.0, 4.0]);
    }

    /** `Add` sozinho já cobre o caso de juntar duas Mixture, cada uma válida só numa faixa de
    índices e ZERO na outra (por construção — `zero()` + preencher só a parte que faz sentido),
    sem precisar de um método de "splice" por faixa de índice.
    */
    #[test]
    fn add_combines_two_zero_padded_range_specific_mixtures() {
        let gas_partial = Mixture::new([5.0, 0.0, 0.0], &TEST_SPECIES); // válido só no índice 0
        let liquid_partial = Mixture::new([0.0, 0.0, 9.0], &TEST_SPECIES); // válido só no índice 2
        assert_eq!((gas_partial + liquid_partial).as_array(), [5.0, 0.0, 9.0]);
    }

    #[test]
    fn ideal_gas_pressure_matches_pv_nrt_with_the_framework_gas_constant() {
        let m = Mixture::new([2.0, 0.0, 0.0], &TEST_SPECIES);
        // P = n*R*T/V = 2.0 * GAS_CONSTANT * 300.0 / 100.0
        assert_eq!(m.ideal_gas_pressure(300.0, 100.0).component(0), 2.0 * GAS_CONSTANT * 300.0 / 100.0);
    }

    #[test]
    fn vapor_pressure_matches_antoine_times_mole_fraction() {
        const SPECIES: Species<1> = ["X"];
        let constants = single_component_coefficients();
        let m = Mixture::new([0.5], &SPECIES);
        // avp=bvp=cvp=0 -> exp(0) = 1.0 -> resultado = 1.0 * fração = 0.5
        assert_eq!(m.vapor_pressure(42.0, &constants).component(0), 0.5);
    }

    #[test]
    fn mixture_enthalpy_method_matches_the_free_function() {
        const SPECIES: Species<1> = ["X"];
        let constants = single_component_coefficients();
        let m = Mixture::new([1.0], &SPECIES);
        assert_eq!(m.enthalpy(100.0, 0, &constants), mixture_enthalpy(&[1.0], 100.0, 0, &constants));
    }

    /* Duas reações sobre 3 espécies (X, Y, Z): X + Y → Z (calor 5.0, velocidade fixa 2.0 por volume)
    e 2 Z → 3 X (calor 0.0, velocidade fixa 1.0 por volume).
    */
    fn test_reactions() -> Reactions<3> {
        Reactions::new(&TEST_SPECIES).add("X + Y -> Z", 5.0, |_, _| 2.0).add("2 Z -> 3 X", 0.0, |_, _| 1.0)
    }

    #[test]
    fn reactions_species_rates_sum_the_stoichiometry_of_every_reaction() {
        let outcome = test_reactions().at(300.0, &Mixture::zero(&TEST_SPECIES), 1.0);
        // X: -1*2 + 3*1 = 1.0 | Y: -1*2 + 0*1 = -2.0 | Z: 1*2 + -2*1 = 0.0
        assert_eq!(outcome.species_rates.as_array(), [1.0, -2.0, 0.0]);
    }

    #[test]
    fn reactions_heat_sums_heat_times_rate_of_every_reaction() {
        let outcome = test_reactions().at(300.0, &Mixture::zero(&TEST_SPECIES), 1.0);
        assert_eq!(outcome.heat, 10.0);
    }

    #[test]
    fn reactions_scale_the_per_volume_rate_by_the_volume() {
        let outcome = test_reactions().at(300.0, &Mixture::zero(&TEST_SPECIES), 10.0);
        assert_eq!(outcome.heat, 100.0);
        assert_eq!(outcome.species_rates.as_array(), [10.0, -20.0, 0.0]);
    }

    #[test]
    fn reactions_hand_the_temperature_and_partial_pressures_to_the_rate_formula() {
        let reactions = Reactions::new(&TEST_SPECIES).add("X -> Y", 0.0, |temperature_k, p| temperature_k * p.get("X"));
        let outcome = reactions.at(10.0, &Mixture::new([3.0, 0.0, 0.0], &TEST_SPECIES), 1.0);
        // velocidade = 10 * 3 = 30: X perde 30, Y ganha 30
        assert_eq!(outcome.species_rates.as_array(), [-30.0, 30.0, 0.0]);
    }

    #[test]
    #[should_panic(expected = "não está no catálogo")]
    fn reactions_panic_on_an_unknown_species_in_the_equation() {
        Reactions::new(&TEST_SPECIES).add("X + Q -> Z", 0.0, |_, _| 1.0);
    }
}
