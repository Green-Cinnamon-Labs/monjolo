# Alternativa 1 — Stream como campo injetado (`@Autowired` num campo)

Ideia central: a Stream deixa de ser algo que cada MÉTODO pede (`#[need(...)]` empilhado, repetido em cada método que precisa dela) e vira um CAMPO do struct, resolvido uma única vez quando a unidade é construída — exatamente como um `@Autowired private Stream inbound;` no Spring: você declara o campo, o container resolve na hora de montar o bean, e todo método da classe usa `this.inbound` à vontade, sem pedir de novo.

## Como fica (pseudocódigo, não compila)

Hoje, em `separator.rs`, a composição/vazão que entram no separador chegam como parâmetros soltos, redeclarados a cada método que precisa deles:

```rust
#[monjolo::tasks]
impl Separator {
    #[need(key = "reactor.temperature")]
    #[offer(key = "separator.temperature")]
    // ... mais 6 offers
    pub(crate) fn physical_state(&self, reactor_temperature: f64) -> (...) { ... }

    // um método DIFERENTE, que também precisa de reactor.temperature,
    // precisaria declarar #[need(key = "reactor.temperature")] DE NOVO
}
```

Com um campo injetado, a `Stream` (vazão + composição da corrente que entra no separador, vindo do reator) vira parte do PRÓPRIO struct:

```rust
#[monjolo::dynamic_model(tasks)]
pub struct Separator {
    #[inject(stream = "reactor.outlet")]
    inbound: Stream,

    #[state]
    #[config(...)]
    #[offer(...)]
    liquid: [f64; 5],

    constants: TepConstants,
}

#[monjolo::tasks]
impl Separator {
    #[offer(key = "separator.temperature")]
    #[offer(key = "separator.pressure")]
    // ... offers continuam (isso é o que ESTE método produz)
    pub(crate) fn physical_state(&self) -> (...) {
        let composicao = self.inbound.composition(); // [f64; N], N = o que fizer sentido pra essa stream
        let vazao = self.inbound.flow();
        // ... física normal daqui pra frente
    }

    // um SEGUNDO método que também precisasse da mesma stream
    // simplesmente usa self.inbound de novo — zero redeclaração
    #[offer(key = "separator.algo_mais")]
    fn outro_calculo(&self) -> f64 {
        self.inbound.composition()[2] * 2.0
    }
}
```

## O que precisaria mudar de verdade

Um atributo NOVO de campo (`#[inject(stream = "...")]`, nome ilustrativo) na macro `#[dynamic_model(...)]` (`monjolo-macros/dynamic_model.rs`), ao lado de `#[state]`/`#[config]`/`#[offer]`/`#[need]` que já existem — na hora de gerar o `new()` da unidade, ele resolveria a Stream (via `subscribe()`/`need_*`, o mesmo mecanismo de sempre, só que devolvendo um objeto `Stream` em vez de um `Proxy` cru) e guardaria como campo. `Stream` em si seria um tipo novo do framework (`monjolo::stream::Stream` ou similar) — um pequeno wrapper com `.flow()`/`.composition()`, por trás dos mesmos `Proxy`s de sempre.

## Trade-offs

**A favor:** resolve exatamente a reclamação — declara uma vez, usa em todo método, sem repetir `#[need]`. Também deixa o `impl` mais limpo: métodos que não mexem com a Stream nem sabem que ela existe.

**Contra:** ainda não resolve a pergunta "quem é DONA da Stream" — ela continua sendo, por trás dos panos, "reactor oferece, separator pede" (a mesma assimetria produtor/consumidor de hoje), só que empacotada num campo em vez de espalhada em parâmetros. Pra quem quer a Stream como uma coisa "entre" as duas unidades, sem dono, isso sozinho não chega lá — ver `alt2-stream-como-servico.md`.
