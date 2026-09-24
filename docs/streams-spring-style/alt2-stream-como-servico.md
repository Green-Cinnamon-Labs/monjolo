# Alternativa 2 — Stream como serviço compartilhado (`@Service` entre duas classes)

Ideia central: a Stream deixa de "pertencer" a quem a produz — ela vira sua PRÓPRIA classe auto-registrada, exatamente como `#[actuator(...)]`/`#[sensor(...)]` já são hoje (um `inventory::submit!` próprio, construída uma vez no boot) — e as DUAS unidades que ela conecta (quem escreve, quem lê) recebem uma referência a ela via injeção (o mecanismo do `alt1-campo-injetado.md`, aplicado aqui por baixo). É o equivalente a um `@Service` do Spring que duas outras classes diferentes `@Autowired`am — nenhuma das duas "é dona" dele, as duas só o usam.

## Como fica (pseudocódigo, não compila)

Primeiro, a Stream vira uma declaração de primeira classe, parecida com `#[actuator(key = "...")]` — só que em vez de um valor único, carrega vazão + composição, com os componentes que fazem sentido pra ESSA stream (ver a tabela em `docs/10-streams.md` — stream 4 tem A/B/C, não os 8):

```rust
#[monjolo::stream(key = "flows.stream4", components = ["a", "b", "c"])]
struct Stream4;
```

Isso geraria, por baixo, os mesmos dois catálogos que `Actuator`/`Sensor` já usam — um lado ESCRITOR (quem publica vazão/composição) e um lado LEITOR (quem lê) — sem que `Feed` ou `Stripper` precisem saber como isso é implementado, só que existe uma "Stream4" endereçável por nome.

`Feed` (quem produz) injeta o lado escritor:

```rust
#[monjolo::dynamic_model(tasks)]
pub struct Feed {
    #[inject(stream_writer = "flows.stream4")]
    ac_feed: StreamWriter,
    // ...
}

#[monjolo::tasks]
impl Feed {
    #[need(key = "valve.feed_ac.position")]
    fn ac_feed_flow(&self, position: f64) {
        self.ac_feed.set_flow(position * FEED_AC_RANGE / 100.0);
        self.ac_feed.set_composition([0.4850, 0.0050, 0.5100]); // nominal, por enquanto
    }
}
```

`Stripper` (quem consome) injeta o lado leitor:

```rust
#[monjolo::dynamic_model(tasks)]
pub struct Stripper {
    #[inject(stream_reader = "flows.stream4")]
    ac_feed: StreamReader,
    // ...
}

#[monjolo::tasks]
impl Stripper {
    #[offer(...)]
    fn flash_split(&self) -> (...) {
        let composicao = self.ac_feed.composition(); // [f64; 3], só A/B/C
        let vazao = self.ac_feed.flow();
        // ...
    }
}
```

## O que precisaria mudar de verdade

Isso é bem mais trabalho que a Alternativa 1: precisaria de (a) um tipo `Stream`/`StreamWriter`/`StreamReader` novo no framework, com catálogo próprio no `StateRegistry` (um terceiro catálogo, ao lado de `actuator_catalog`/`sensor_catalog`); (b) uma macro nova (`#[monjolo::stream(...)]`), que gera o registro; (c) a Alternativa 1 (`#[inject(...)]`) já pronta por baixo, já que é assim que `Feed`/`Stripper` pegariam suas referências. Ou seja: Alternativa 2 CONTÉM a Alternativa 1, não é uma opção separada dela.

## Trade-offs

**A favor:** é a forma mais fiel ao que foi descrito ("uma coisa que é uma stream ENTRE duas unidades, passada como um serviço") — nenhuma das duas unidades é dona, e cada Stream real do processo (tabela em `docs/10-streams.md`) vira uma declaração explícita, com só os componentes que ela de fato carrega. Também abre a porta natural pro Disturbance (pausado) se encaixar bem depois: um distúrbio seria só mais um `StreamWriter` que intercepta/altera antes de reoferecer — a MESMA peça, reaproveitada.

**Contra:** bem mais superfície nova de framework (tipo + catálogo + macro), tudo por cima da Alternativa 1, que ainda nem existe. Onze streams reais (tabela do doc 10) significariam onze declarações `#[monjolo::stream(...)]` — vale conferir se isso não vira boilerplate demais antes de comprar essa direção inteira.
