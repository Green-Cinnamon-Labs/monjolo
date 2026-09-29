# Mixture — uma primitiva de framework pra composição química, não um helper de `Reactor`

Este documento registra a filosofia por trás de `monjolo::chemistry::Mixture<N>` (implementado de verdade em `monjolo/chemistry.rs`, com testes) — o tipo nasceu de uma conversa sobre limpar `Reactor::physical_state()` (`tep-plant`), mas a decisão foi que ele não deveria viver lá: é um primitivo do FRAMEWORK, útil pra qualquer planta montada sobre `monjolo` que tenha "vasos com misturas de componentes químicos dentro" — o que é basicamente todo processo químico, não uma particularidade do TEP.

## O problema que motivou isto

Em toda unidade de `tep-plant` (`Reactor`, `Separator`, `Stripper`, `Compressor`), a mesma forma de código se repete: um array `[f64; 8]` (às vezes moles, às vezes fração molar — nada no TIPO diz qual) e um `for i in 0..8` reimplementando a mesma operação genérica (somar, normalizar por total, produto escalar, aplicar uma fórmula por componente) com dados diferentes a cada chamada. Isso não é física — é aritmética vetorial repetida, sem nome, sem tipo que capture a diferença entre "isto é moles" e "isto é fração molar" ou entre "isto é vapor" e "isto é líquido".

## Os cinco princípios (conversa de 2026-09-24, registrados como o autor os colocou)

**1. Uma mistura existe independente de fase.** Se há um vaso, tudo que está nele está misturado, mesmo que haja separação de fase — vapor e líquido não são dois objetos fundamentalmente diferentes, são DUAS VISÕES (ou dois estados) da MESMA mistura. `self.vapor()`/`self.liquid()` deveriam devolver `Mixture` diretamente, não array cru que o chamador embrulha depois. **Status: parcialmente resolvido.** `Mixture::add` já deixa "vapor + líquido = inventário total do vaso" funcionar sem método especial (a fase resultante vira `Phase::Mixed` automaticamente) — ver o teste `add_combines_two_zero_padded_range_specific_mixtures_without_needing_a_splice_method`. O que ainda NÃO está feito: `Reactor::vapor()`/`liquid()` (os getters gerados por `#[state]`) continuam devolvendo `[f64; N]` cru, não `Mixture` — integrar isso em `tep-plant` de verdade é o próximo passo, não feito ainda.

**2. Zero não deveria precisar ser dito.** Se o usuário cria uma mistura sem informar nada, ela começa em zero — ele não deveria escrever `[0.0; N]` na mão. **Status: resolvido.** `Mixture::zero(phase, species)` — só fase e catálogo de nome, os valores vêm de graça.

**3. Nada se cria, tudo se transforma — exceto no Feed.** A grande maioria das `Mixture` do sistema deveria nascer de OUTRA `Mixture` já existente (soma, subtração, escala, normalização), nunca de um array digitado à mão. O único lugar onde massa genuinamente entra no sistema vindo de fora é o feed externo de uma planta. **Status: parcialmente resolvido — é convenção, não regra imposta pelo tipo.** `Mixture::new(valores, ...)` continua existindo (não dá pra eliminar sem um construtor especial só pro Feed, que não foi desenhado ainda) — é documentado como "a fronteira", mas nada no compilador impede alguém de chamar `new` em qualquer lugar. Ficou registrado como próxima pergunta de design, não resolvido agora.

**4. A interface tem que responder "o que tem aqui, em que fase, quanto isso representa do todo, e o nome de cada coisa".** **Status: resolvido pro que já foi definido.** `.component(i)`, `.phase()`, `.fraction_of(&whole)`, `.name(i)` — todos existem. `name(i)` depende de um catálogo `Species<N>` (`&'static [&'static str; N]`) fornecido por quem constrói a `Mixture` — o catálogo em si (ex.: `["A","B",...,"H"]` pro TEP) ainda não foi definido em `tep-plant`, só o MECANISMO pra carregar um existe.

**5. Isto é uma peça de framework, não um helper de uma unidade.** **Status: resolvido pela localização.** Vive em `monjolo::chemistry` (atrás da feature `chemistry`, mesmo padrão de `mixture_enthalpy`/`liquid_density`/`Coefficients<N>`, que já eram genéricos sobre N — não específicos do TEP). Nenhuma planta é obrigada a usar `Mixture`; quem quiser continuar com array cru pode.

## O que existe de verdade, hoje (`monjolo/monjolo/chemistry.rs`)

- `Phase` (`Vapor`/`Liquid`/`Mixed`) e `Species<N>` (catálogo de nomes, `&'static`).
- `Mixture<const N: usize>` — `values: [f64; N]` + `phase` + referência ao catálogo de nomes.
- Construtores: `zero(phase, species)`, `new(values, phase, species)`.
- Leitura: `component(i)`, `name(i)`, `phase()`, `as_array()`, `total()`.
- Transformações (todas devolvem uma `Mixture` NOVA, nenhuma muta `self`): `mole_fractions()`, `scaled_by(fator)`, `dot(&outro_array)`, `fraction_of(&outra)`, `ideal_gas_pressure(t_k, v, r)`, `vapor_pressure(t, &Coefficients)`, `enthalpy(t, ity, &Coefficients)` (delega pra `mixture_enthalpy` já existente).
- Operadores: `Add` (fase preservada se iguais, `Mixed` se diferentes), `Sub` (preserva a fase de `self`).
- 15 testes cobrindo cada operação, incluindo a prova de que `Add` sozinho substitui um `splice`/merge-por-faixa-de-índice que apareceu num rascunho anterior (`tep-plant/docs/streams-spring-style/reactor-limpo-hipotetico.rs`).

## O que NÃO foi feito ainda (propositalmente — isto é o começo, não o fim)

- Nenhuma unidade de `tep-plant` foi migrada pra usar `Mixture` de verdade — o rascunho em `docs/streams-spring-style/` (no repo `tep-plant`) mostra como `Reactor::physical_state`/`flow_to_separator`/`mass_and_energy_balance` ficariam, mas é hipotético, não compila, não foi aplicado.
- Catálogo `Species<8>` do TEP (`["A",...,"H"]`) ainda não foi definido em `tep-plant/src/physics/constants.rs`.
- Nenhum construtor "só pro Feed" existe pra dar dentes de verdade ao princípio 3 — hoje é só convenção documentada.
- A pergunta de como `#[need]`/`#[offer]` (a macro de `monjolo-macros`) publicaria/consumiria uma `Mixture` inteira via StateRegistry (que só entende `f64` por chave) continua em aberto — isso é o mesmo problema de fundo da discussão de Stream/Flow (`tep-plant/docs/10-streams.md`), ainda não resolvido.

## Atualização (2026-09-26): `Phase` saiu, e a receita de reações ganhou um builder

Nenhum cálculo olhava a fase de uma `Mixture` (ela só era gravada na criação e lida em `Add` e nos testes), e ainda gerou o abuso de `Phase::Mixed` para "não se aplica", então `Phase` foi removido: `Mixture::new(valores, &espécies)`. Quem dá o significado de vapor ou líquido é o nome da variável de quem usa. Um cálculo que depende do estado físico (gás ideal para os componentes não condensáveis, Antoine para os condensáveis) já era escolhido pelo método, não pela fase carregada. Os princípios 1 e 4 acima continuam valendo pelo resto da interface (soma, nome de cada componente, fração do todo); só a parte "em que fase está" deixou de existir como dado.

`ReactionScheme` e `Reaction` (uma tabela de índices mais um vetor de taxas) viraram `Reactions` e `ReactionOutcome`: `Reactions::new(&espécies).add("A + C + D -> G", calor, |T, p| ...)` monta as reações pelo NOME das espécies, cada uma com a própria receita, o próprio calor e a própria fórmula de velocidade, e `at(T, pressões parciais, volume)` devolve um `ReactionOutcome` (`species_rates` como `Mixture` e `heat`). A constante dos gases (`GAS_CONSTANT`) e a fórmula de Arrhenius (`arrhenius`) também moram em `monjolo::chemistry`, e `Mixture::get("A")` lê uma espécie pelo nome.
