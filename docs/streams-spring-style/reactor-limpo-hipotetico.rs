/* HIPOTÉTICO — NÃO COMPILA, NÃO FAZ PARTE DO CRATE. Duas ideias empilhadas: (1) estilo V2 — método
sem atributo/parâmetro/retorno, só `self.get()`/`self.set(...)` (ver comentário mais abaixo sobre o
preço disso pro `sort_phase_a`); (2) `monjolo::chemistry::Mixture<8>` — agora o tipo DE VERDADE
(implementado, testado, ver `monjolo/chemistry.rs` e `monjolo/docs/mixture-design.md`), não mais um
struct local inventado aqui. O array `[f64; 8]` que aparecia solto em toda parte, sempre
reimplementando a mesma aritmética vetorial por loop, vira esse tipo com métodos nomeados.

Onde `Mixture` ajuda de verdade: soma, normalização (moles → fração molar), produto escalar (peso
molecular), combinação linear (balanço de massa `entrada - saída + reação`) — essas SÃO a mesma
operação genérica repetida com dados diferentes, então merecem um nome e um lugar só. Onde `Mixture`
NÃO ajuda (e por isso não aparece em `heat_exchange`/cinética de reação): fórmulas que não são
"aplique isto em cada um dos 8 componentes", são regras específicas por reação/mecanismo — forçar
isso a virar método de `Mixture` só esconderia a física atrás de um nome genérico demais.

Achado ao migrar pro tipo de verdade: o `splice(a, b, 3)` que uma versão anterior deste arquivo
inventava (juntar duas Mixture por faixa de índice) era desnecessário — `Add` sozinho já dá o
resultado certo quando as duas entradas já estão zero-preenchidas fora da própria faixa (que é
exatamente o caso aqui: `ideal_gas_pressure` já é zero em 3..8, `vapor_pressure` já é zero em 0..3),
porque somar zero com o valor real dá o valor real. Ver o teste
`add_combines_two_zero_padded_range_specific_mixtures_without_needing_a_splice_method` em
`monjolo/chemistry.rs`.
*/

use crate::physics::constants::TepConstants;
use monjolo::chemistry::{liquid_density, temperature_from_enthalpy, Mixture, Phase, Species};

const REACTOR_VOLUME: f64 = 1300.0;
const GAS_CONSTANT: f64 = 998.9;
const REACTION_ENTHALPIES: [f64; 2] = [0.06899381054, 0.05];
const REACTION_FACTOR_1_NOMINAL: f64 = 1.0;
const REACTION_FACTOR_2_NOMINAL: f64 = 1.0;
const TEMPERATURE_SEED: f64 = 120.0;
const REACTOR_COOLING_WATER_RETURN: f64 = 94.59927549;

/* Catálogo de nomes dos 8 componentes do TEP — ainda não existe de verdade em
`tep-plant/src/physics/constants.rs` (ver "o que não foi feito" em `monjolo/docs/mixture-design.md`);
teria que morar lá, não aqui, quando isso deixar de ser hipotético.
*/
const TEP_SPECIES: Species<8> = ["A", "B", "C", "D", "E", "F", "G", "H"];

/* Cada relação do Reactor com OUTRA unidade vira um tipo nomeado, não mais um punhado de campo
solto (`agitator_speed`, `separator_pressure`, `compressor_vapor`...) que não revela de onde vem.
O nome do campo no Reactor (`agitator`, `separator`, `compressor`) já É a relação — bate 1:1 com
cada conexão física do P&ID (eixo SC, linha de pressão, stream 6): quem olha o struct enxerga com
quantas e quais unidades o Reactor se relaciona, sem contar `#[need]` solto um por um.
*/
struct AgitatorLink {
    #[need(key = "agitator.speed")]
    speed: f64,
}

struct SeparatorLink {
    #[need(key = "separator.pressure")]
    pressure: f64,
}

struct CompressorLink {
    #[need(prefix = "compressor.vapor_composition", components = ["0", "1", "2", "3", "4", "5", "6", "7"])]
    vapor: [f64; 8],
    #[need(key = "compressor.temperature")]
    temperature: f64,
    #[need(key = "flows.stream_flow.6")]
    recycle_flow: f64,
}

#[monjolo::dynamic_model(tasks)]
pub struct Reactor {
    /* Atributos do próprio Reactor — o que ele É, não o que recebe de fora. */
    #[state]
    #[config(prefix = "state.reactor_vapor", components = ["A", "B", "C"])]
    #[offer(prefix = "reactor.state", components = ["vapor_a", "vapor_b", "vapor_c"])]
    vapor: [f64; 3],

    #[state]
    #[config(prefix = "state.reactor_vapor", components = ["D", "E", "F", "G", "H"])]
    #[offer(prefix = "reactor.state", components = ["liquid_d", "liquid_e", "liquid_f", "liquid_g", "liquid_h"])]
    liquid: [f64; 5],

    #[state]
    #[config(key = "state.reactor.energy")]
    #[offer(key = "reactor.state.enthalpy")]
    enthalpy: f64,

    /* Relações — uma unidade externa por campo, não uma chave por campo. */
    agitator: AgitatorLink,
    separator: SeparatorLink,
    compressor: CompressorLink,

    #[offer(key = "reactor.temperature")]
    temperature: f64,
    #[offer(key = "reactor.temperature_k")]
    temperature_k: f64,
    #[offer(key = "reactor.pressure")]
    pressure: f64,
    #[offer(key = "reactor.liquid_volume")]
    liquid_volume: f64,
    #[offer(key = "reactor.liquid_density")]
    liquid_density: f64,
    #[offer(key = "reactor.vapor_volume")]
    vapor_volume: f64,
    #[offer(key = "reactor.total_vapor_kmol")]
    total_vapor_kmol: f64,
    #[offer(key = "reactor.heat_of_reaction")]
    heat_of_reaction: f64,
    /* Os quatro campos abaixo eram `[f64; 8]` cru — viravam `Mixture` de novo toda vez que outro
    método precisava ler (repetindo fase/espécie em 3 lugares). Guardando `Mixture<8>` direto no
    campo, `self.vapor_composition()` já devolve o tipo certo, sem reconstruir nada. Em aberto,
    igual já registrado em `monjolo/docs/mixture-design.md`: não está decidido se `#[offer(prefix,
    components)]` sabe publicar uma `Mixture` inteira via StateRegistry (que só entende `f64` solto
    por chave) — aqui assume que sim, só pra mostrar o ganho no resto do código.
    */
    #[offer(prefix = "reactor.liquid_composition", components = ["a", "b", "c", "d", "e", "f", "g", "h"])]
    liquid_composition: Mixture<8>,
    #[offer(prefix = "reactor.vapor_composition", components = ["a", "b", "c", "d", "e", "f", "g", "h"])]
    vapor_composition: Mixture<8>,
    #[offer(prefix = "reactor.vapor_kmol", components = ["a", "b", "c", "d", "e", "f", "g", "h"])]
    vapor_kmol: Mixture<8>,
    #[offer(prefix = "reactor.reaction_rates", components = ["a", "b", "c", "d", "e", "f", "g", "h"])]
    reaction_rates: Mixture<8>,

    #[offer(key = "flows.stream_flow.7")]
    outlet_flow: f64,

    #[offer(key = "heat.reactor_heat")]
    reactor_heat: f64,
    #[offer(key = "heat.reactor_cooling_water_return")]
    cooling_water_return: f64,

    #[offer(prefix = "reactor.state", components = ["vapor_a.derivative", "vapor_b.derivative", "vapor_c.derivative"])]
    vapor_derivative: [f64; 3],
    #[offer(prefix = "reactor.state", components = ["liquid_d.derivative", "liquid_e.derivative", "liquid_f.derivative", "liquid_g.derivative", "liquid_h.derivative"])]
    liquid_derivative: [f64; 5],
    #[offer(key = "reactor.state.enthalpy.derivative")]
    enthalpy_derivative: f64,

    #[offer(key = "xmeas.reactor.pressure")]
    xmeas_pressure: f64,
    #[offer(key = "xmeas.reactor.level")]
    xmeas_level: f64,
    #[offer(key = "xmeas.reactor.temperature")]
    xmeas_temperature: f64,
    #[offer(key = "xmeas.reactor.cooling_water_outlet_temperature")]
    xmeas_cooling_water_outlet_temperature: f64,

    constants: TepConstants,
}

#[monjolo::tasks]
impl Reactor {
    fn physical_state(&self) {
        /* vapor()/liquid() são [f64;3]/[f64;5] (guardados separados por causa da chave de config) —
        `Mixture::at` embute cada um na posição certa de um total de 8, zero no resto, sem o
        `from_fn`+`if` que uma versão anterior deste arquivo escrevia à mão.
        */
        let vapor = Mixture::at(0, &self.vapor(), Phase::Vapor, &TEP_SPECIES);
        let liquid = Mixture::at(3, &self.liquid(), Phase::Liquid, &TEP_SPECIES);

        let liquid_composition = liquid.mole_fractions();

        let specific_enthalpy = self.enthalpy() / liquid.total();
        let temperature = temperature_from_enthalpy(&liquid_composition.as_array(), TEMPERATURE_SEED, specific_enthalpy, 0, &self.constants);
        let temperature_k = temperature + 273.15;
        let density = liquid_density(&liquid_composition.as_array(), temperature, &self.constants);
        let volume_liquid = liquid.total() / density;
        let volume_vapor = REACTOR_VOLUME - volume_liquid;

        /* Duas fórmulas de pressão parcial diferentes (gás ideal pra A/B/C, Antoine pra D-H) — cada
        Mixture só é válida na SUA faixa, zero na outra por construção (`vapor`/`liquid_composition`
        já nasceram assim via `Mixture::at`) — então `+` sozinho já junta as duas certo, sem
        `splice` nenhum (ver achado no comentário do topo do arquivo).
        */
        let gas_partial = vapor.ideal_gas_pressure(temperature_k, volume_vapor, GAS_CONSTANT);
        let liquid_partial = liquid_composition.vapor_pressure(temperature, &self.constants);
        let partial_pressures = gas_partial + liquid_partial;
        let pressure = partial_pressures.total();

        let vapor_composition = partial_pressures.mole_fractions();
        let total_vapor_moles = pressure * volume_vapor / GAS_CONSTANT / temperature_k;
        /* CORRIGIDO: uma versão anterior deste rascunho fazia `vapor + vapor_composition.scaled_by(..)`,
        o que contava A/B/C duas vezes (a composição também é não-zero em 0..3). Como P_i = n_i·R·T/V,
        `total_vapor_moles * vapor_composition` já É o vetor de moles de vapor inteiro.
        */
        let vapor_moles = vapor_composition.scaled_by(total_vapor_moles);

        /* Cinética de reação: NÃO é "aplique a mesma fórmula nos 8 componentes" — é regra própria
        por reação, cada uma com constantes/expoentes diferentes. Fica fora de `Mixture` de propósito.
        */
        let mut rates = [0.0f64; 4];
        rates[0] = (31.5859536 - 40000.0 / 1.987 / temperature_k).exp() * REACTION_FACTOR_1_NOMINAL;
        rates[1] = (3.00094014 - 20000.0 / 1.987 / temperature_k).exp() * REACTION_FACTOR_2_NOMINAL;
        rates[2] = (53.4060443 - 60000.0 / 1.987 / temperature_k).exp();
        rates[3] = rates[2] * 0.767488334;
        if partial_pressures.component(0) > 0.0 && partial_pressures.component(2) > 0.0 {
            let rf1 = partial_pressures.component(0).powf(1.1544);
            let rf2 = partial_pressures.component(2).powf(0.3735);
            rates[0] *= rf1 * rf2 * partial_pressures.component(3);
            rates[1] *= rf1 * rf2 * partial_pressures.component(4);
        } else {
            rates[0] = 0.0;
            rates[1] = 0.0;
        }
        rates[2] *= partial_pressures.component(0) * partial_pressures.component(4);
        rates[3] *= partial_pressures.component(0) * partial_pressures.component(3);
        for r in rates.iter_mut() {
            *r *= volume_vapor;
        }

        let mut reaction_rates_raw = [0.0f64; 8];
        reaction_rates_raw[0] = -rates[0] - rates[1] - rates[2];
        reaction_rates_raw[2] = -rates[0] - rates[1];
        reaction_rates_raw[3] = -rates[0] - 1.5 * rates[3];
        reaction_rates_raw[4] = -rates[1] - rates[2];
        reaction_rates_raw[5] = rates[2] + rates[3];
        reaction_rates_raw[6] = rates[0];
        reaction_rates_raw[7] = rates[1];
        let heat_of_reaction = rates[0] * REACTION_ENTHALPIES[0] + rates[1] * REACTION_ENTHALPIES[1];

        self.set_temperature(temperature);
        self.set_temperature_k(temperature_k);
        self.set_pressure(pressure);
        self.set_liquid_volume(volume_liquid);
        self.set_liquid_density(density);
        self.set_vapor_volume(volume_vapor);
        self.set_total_vapor_kmol(total_vapor_moles);
        self.set_heat_of_reaction(heat_of_reaction);
        self.set_liquid_composition(liquid_composition);
        self.set_vapor_composition(vapor_composition);
        self.set_vapor_kmol(vapor_moles);
        self.set_reaction_rates(Mixture::new(reaction_rates_raw, Phase::Mixed, &TEP_SPECIES));
    }

    fn flow_to_separator(&self) {
        let mol_weight = self.vapor_composition().dot(&self.constants.xmw);
        self.set_outlet_flow(4574.21 * (self.pressure() - self.separator.pressure).max(0.0).sqrt() * (1.0 - 0.25 * 0.0) / mol_weight);
    }

    /* Sem array de 8 componentes aqui — Mixture não se aplica, fica exatamente como no estilo V2. */
    fn heat_exchange(&self) {
        let agitation_factor = (self.agitator.speed + 150.0) / 100.0;
        let level = self.liquid_volume() / 7.8;
        let uar_level = if level > 50.0 { 1.0 } else if level < 10.0 { 0.0 } else { 0.025 * level - 0.25 };
        let uar = uar_level * (-0.5 * agitation_factor * agitation_factor + 2.75 * agitation_factor - 2.5) * 855490e-6;

        self.set_cooling_water_return(REACTOR_COOLING_WATER_RETURN);
        self.set_reactor_heat(uar * (self.cooling_water_return() - self.temperature()) * (1.0 - 0.35 * 0.0));
    }

    fn mass_and_energy_balance(&self) {
        /* Balanço de massa = entrada - saída + reação, componente a componente — é EXATAMENTE o
        que `Mixture::scaled_by`/`Sub`/`Add` existem pra expressar, sem loop nenhum. `reaction_rates`
        não é vapor nem líquido de verdade (é uma taxa, não uma composição) — `Phase::Mixed` usado
        aqui como "não se aplica", uma aresta solta que `monjolo/docs/mixture-design.md` já registra
        como pergunta em aberto (nem toda grandeza de 8 posições tem fase de verdade).
        */
        let inflow = Mixture::new(self.compressor.vapor, Phase::Vapor, &TEP_SPECIES).scaled_by(self.compressor.recycle_flow);
        let outflow = self.vapor_composition().scaled_by(self.outlet_flow());
        let reaction = self.reaction_rates();
        let derivative = (inflow - outflow + reaction).as_array();

        self.set_vapor_derivative([derivative[0], derivative[1], derivative[2]]);
        self.set_liquid_derivative([derivative[3], derivative[4], derivative[5], derivative[6], derivative[7]]);
        self.set_enthalpy_derivative(
            Mixture::new(self.compressor.vapor, Phase::Vapor, &TEP_SPECIES).enthalpy(self.compressor.temperature, 1, &self.constants)
                * self.compressor.recycle_flow
                - self.vapor_composition().enthalpy(self.temperature(), 1, &self.constants) * self.outlet_flow()
                + self.heat_of_reaction()
                + self.reactor_heat(),
        );
    }

    fn xmeas_readings(&self) {
        self.set_xmeas_pressure((self.pressure() - 760.0) / 760.0 * 101.325);
        self.set_xmeas_level((self.liquid_volume() - 84.6) / 666.7 * 100.0);
        self.set_xmeas_temperature(self.temperature());
        self.set_xmeas_cooling_water_outlet_temperature(self.cooling_water_return());
    }
}

/* Sem #[need]/#[offer] nenhum (estilo V2), sort_phase_a perde o insumo pra ordenar os 5 métodos
acima sozinho — precisaria ou (a) inferir por inspeção do corpo (self.getter()/self.set_setter())
ou (b) um evaluate() escrito à mão, nesta ordem (physical_state -> flow_to_separator/heat_exchange
-> mass_and_energy_balance -> xmeas_readings). Nenhuma das duas está implementada.
*/
