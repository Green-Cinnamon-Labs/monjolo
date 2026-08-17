# Macros procedurais para Actuator/Sensor/Controller/DynamicModel — design mínimo

Documento de investigação, não de implementação. Vive só nesta branch (`feat/proc-macro-components`)
enquanto o desenho é avaliado — se aprovado, o conteúdo relevante migra pra `CONTRIBUTING.md` como
artigo numerado, na hora de implementar de verdade.

## Resumo executivo

- **Actuator, Sensor, Controller**: já têm um tipo genérico em runtime (`actuator::model::Actuator`,
  `sensor::model::Sensor`, `controller::model::Controller`). A macro, pra esses três, é *puro
  açúcar sintático* sobre uma API que já existe e já funciona — risco baixo, ganho alto.
- **DynamicModel**: não tem (nem precisa ganhar) um tipo genérico novo em runtime. A macro teria que
  gerar o `struct` + `impl DynamicModel` inteiros — o mesmo código que `Reactor`/`Separator`/
  `Stripper`/`Compressor` já escrevem à mão hoje. É o caso mais valioso (é onde mora a maior parte
  do boilerplate real) e também o mais arriscado de acertar de primeira.
- **O que isso NÃO resolve, de propósito**: descoberta automática (a macro não faz `build_tep()`
  parar de chamar cada função explicitamente — isso é um problema *diferente*, já registrado como
  item em aberto no `CONTRIBUTING.md`, Art. 6.2 §1, candidatos `inventory`/`linkme`/`ctor`). Aqui só
  se reduz o que sobra *depois* de decidir chamar cada um.
- **Consequência estrutural inevitável**: `monjolo` precisa virar dois crates —
  `monjolo` (runtime) e `monjolo-macros` (`proc-macro = true`, macro-only, sem exceção estável em
  Rust) — com `monjolo` reexportando as macros pra quem usa só precisar de uma dependência.

---

## 1. Actuator — o caso mais limpo

### Hoje (`tep-plant/src/subsystems/actuators.rs`)

```rust
pub fn feed_d(registry: &mut StateRegistry) -> Rc<Actuator> {
    Actuator::new(registry, "valve.feed_d.position", |command, position| {
        let tau = 8.0 / 3600.0;
        (command - position) / tau
    })
}
```

### Proposto

```rust
#[actuator(key = "valve.feed_d.position")]
fn feed_d(command: f64, position: f64) -> f64 {
    let tau = 8.0 / 3600.0;
    (command - position) / tau
}
```

### O que a macro gera

```rust
pub fn feed_d(registry: &mut StateRegistry) -> Rc<Actuator> {
    fn __feed_d_dynamics(command: f64, position: f64) -> f64 {
        let tau = 8.0 / 3600.0;
        (command - position) / tau
    }
    Actuator::new(registry, "valve.feed_d.position", __feed_d_dynamics)
}
```

### Por que funciona sem atrito nenhum

`fn(f64, f64) -> f64` (um item de função livre, sem captura nenhuma) já satisfaz `impl Fn(f64, f64)
-> f64 + 'static` automaticamente — não precisa de closure, `move`, nem lifetime especial. A macro
só precisa: (1) renomear a função original pra um nome interno, (2) gerar uma função pública com o
nome original que chama `Actuator::new(registry, key, <nome interno>)`. É uma transformação
sintática direta, sem nenhuma questão de ownership/tipo envolvida — o caso mais simples dos quatro,
de longe.

**Bônus de testabilidade**: nada impede a macro de também manter `__feed_d_dynamics` (ou um alias
`pub(crate) fn feed_d_dynamics`) acessível pra teste unitário direto — `assert_eq!(feed_d_dynamics(50.0,
0.0), 25.0)`, sem StateRegistry nenhum envolvido. Hoje isso não é possível: testar a física de um
atuador exige `StateRegistry::shared()` + `resolve()` só pra chegar no número.

---

## 2. Sensor — quase tão limpo, mas sem "corpo" natural

Sensor não tem uma "lei física" — só uma chave e um `SensorBehavior` (`Ideal`, `Noisy`, `Hysteresis`,
ou algo customizado). O corpo da função vira o construtor do behavior, não uma fórmula:

### Hoje

```rust
pub fn reactor_pressure(registry: &mut StateRegistry) -> Arc<Sensor> {
    Sensor::new(registry, "reactor.pressure", Box::new(Ideal))
}
```

### Proposto

```rust
#[sensor(key = "reactor.pressure")]
fn reactor_pressure() -> impl SensorBehavior {
    Ideal
}
```

Pra um sensor com ruído, o corpo simplesmente muda — nada de sintaxe nova:

```rust
#[sensor(key = "reactor.temperature")]
fn reactor_temperature() -> impl SensorBehavior {
    Noisy::new(0.01, 42)
}
```

### O que a macro gera

```rust
pub fn reactor_pressure(registry: &mut StateRegistry) -> Arc<Sensor> {
    fn __reactor_pressure_behavior() -> impl SensorBehavior { Ideal }
    Sensor::new(registry, "reactor.pressure", Box::new(__reactor_pressure_behavior()))
}
```

Mesma mecânica do Actuator — função livre sem captura, sem atrito de ownership. A única
particularidade é que "a função computa uma configuração" em vez de "a função é a física" — mas
isso não muda nada na viabilidade técnica, só na leitura.

---

## 3. Controller — o caso mais simples dos quatro

Hoje `Controller` não tem lógica nenhuma (de propósito — `step()`/`update()` continuam em aberto).
Ele só declara nomes. Não sobra corpo de função pra escrever:

### Hoje

```rust
pub fn reactor_pressure_control(registry: &mut StateRegistry) -> Rc<Controller> {
    Controller::new(
        registry,
        "reactor_pressure_control",
        &["reactor.pressure"],
        &["valve.purge.position"],
    )
}
```

### Proposto

```rust
#[controller(
    name = "reactor_pressure_control",
    sensors = ["reactor.pressure"],
    actuators = ["valve.purge.position"],
)]
fn reactor_pressure_control() {}
```

(o corpo vazio incomoda um pouco — ver §6.3, alternativa via `struct` unitária)

### O que a macro gera

```rust
pub fn reactor_pressure_control(registry: &mut StateRegistry) -> Rc<Controller> {
    Controller::new(
        registry,
        "reactor_pressure_control",
        &["reactor.pressure"],
        &["valve.purge.position"],
    )
}
```

Sem nenhuma questão de tipo/ownership: os argumentos da macro (`name`, `sensors`, `actuators`) são
só *tokens* (uma string e duas listas de strings) que a macro lê da própria invocação e reempacota
como a chamada de `Controller::new()` de sempre — não precisa nem olhar o corpo da função.

---

## 4. DynamicModel — o caso difícil (e o que mais vale a pena)

Aqui não existe hoje nenhum tipo genérico em runtime — `Reactor`/`Separator`/`Stripper`/`Compressor`
são cada um seu próprio `struct` com campos `Proxy` nomeados + `impl DynamicModel` escrito à mão. A
macro, pra este caso, precisa gerar *esse mesmo* `struct`+`impl` — não existe atalho via um tipo
genérico pronto como o `Actuator`/`Sensor`/`Controller` já têm.

Duas formas concretas foram avaliadas.

### 4.1 Candidato A — atributo sobre função (espelha o pedido original)

```rust
#[dynamic_model(name = "Compressor")]
fn compressor(
    #[state(seed = "state.compressor_vapor.A")] vapor_a: f64,
    #[state(seed = "state.compressor_vapor.B")] vapor_b: f64,
    // ... 6 mais + energy
    #[need("separator.temperature")] separator_temperature: f64,
) -> CompressorOutputs {
    // física pura, exatamente como hoje, só sem `self.foo.get()/.set()`
    let total_vapor_moles = vapor_a + vapor_b + /* ... */;
    // ...
    CompressorOutputs { temperature, pressure, composition_a, /* ... */ }
}

#[dynamic_model_outputs]
struct CompressorOutputs {
    #[offer("compressor.temperature")] temperature: f64,
    #[offer("compressor.pressure")] pressure: f64,
    #[offer("compressor.vapor_composition.0")] composition_a: f64,
    // ...
}
```

**Atributos em parâmetro de função são sintaticamente válidos aqui só porque a função inteira é
consumida como token stream cru pela macro externa antes do compilador tentar validar qualquer
atributo interno** — fora de uma macro assim, `fn foo(#[state] x: f64)` não compila em Rust estável
(atributo arbitrário em parâmetro é rejeitado, `E0658`). Não é um recurso "normal" da linguagem, é
uma janela que só existe porque a macro reescreve o item inteiro.

**O problema real deste candidato**: a macro que processa `fn compressor(...)` e a macro que
processa `struct CompressorOutputs` são *duas expansões completamente independentes* — uma
proc-macro nunca vê o resultado (nem os atributos) de outro item em outro lugar do crate; macros em
Rust operam por invocação, sem visão global. Então `#[dynamic_model]` não tem como, sozinha,
descobrir que `CompressorOutputs::temperature` corresponde à chave `"compressor.temperature"` — isso
só existe porque `CompressorOutputs` tem sua *própria* macro (`#[dynamic_model_outputs]`) resolvendo
seus próprios campos, separadamente. As duas precisam concordar por *convenção de nome de campo*,
não por informação compartilhada de verdade — funciona, mas é dois pontos de manutenção que têm que
ficar sincronizados manualmente (renomeou um campo, esqueceu do outro lado, erro só aparece como
"unused" ou como um `.set()` que nunca é chamado — não há erro de compilação te avisando).

### 4.2 Candidato B — derive sobre struct (mais alinhado ao mecanismo real da linguagem)

```rust
#[derive(DynamicModel)]
#[dynamic_model(name = "Compressor")]
struct Compressor {
    constants: TepConstants, // campo comum, sem atributo — carregado como está, sem virar Proxy

    #[state(seed = "state.compressor_vapor.A")]
    vapor_a: f64,
    #[state(seed = "state.compressor_vapor.B")]
    vapor_b: f64,
    // ... 6 mais + energy

    #[offer("compressor.temperature")]
    temperature: f64,
    #[offer("compressor.pressure")]
    pressure: f64,
    #[offer("compressor.vapor_composition.0")]
    composition_a: f64,
    // ...

    #[need("separator.temperature")]
    separator_temperature: f64,
}

impl Compressor {
    fn compute(&mut self) {
        // física pura: lê self.vapor_a, self.separator_temperature (já são f64 de verdade aqui,
        // não Proxy) e escreve em self.temperature, self.pressure, self.composition_a etc.
        self.temperature = /* ... */;
        self.pressure = /* ... */;
        // ...
    }
}
```

`#[proc_macro_derive(DynamicModel, attributes(state, offer, need))]` é o mecanismo **oficialmente
suportado** pra atributo-por-campo em Rust — diferente do Candidato A, aqui não há ambiguidade: a
macro declara `state`/`offer`/`need` como "helper attributes" na própria assinatura do derive, e o
compilador aceita esses atributos nos campos sem reclamar de "atributo desconhecido", garantido pela
linguagem, não por acaso de ordem de expansão.

E o problema de correlação do Candidato A desaparece: um único `struct` já contém tanto os `#[state]`/
`#[need]` (entradas) quanto os `#[offer]` (saídas) — uma única macro, uma única passada, sem
depender de dois itens concordando por convenção.

O preço: `compute()` deixa de ser "função pura" (parâmetros → retorno) e vira um método `&mut self`
que lê e escreve campos do próprio struct — mais parecido com o `evaluate()` de hoje (que já lê/
escreve via `self.foo`), só que sobre `f64` direto em vez de `Proxy`. A macro gera, por trás:

```rust
struct Compressor {
    constants: TepConstants,
    vapor_a: f64, vapor_b: f64, /* ... */,      // cache local, não Proxy
    temperature: f64, pressure: f64, /* ... */,  // idem
    separator_temperature: f64,                  // idem

    __proxies: __CompressorProxies, // gerado pela macro, escondido — todos os Proxy reais moram aqui
}

impl DynamicModel for Compressor {
    fn name(&self) -> &str { "Compressor" }

    fn evaluate(&self) {
        // 1. lê cada Proxy pra dentro do campo f64 correspondente (precisa de &mut self por trás de
        //    Cell/RefCell — mesma mutabilidade interior que Proxy já usa)
        // 2. chama self.compute()
        // 3. escreve cada campo #[offer] de volta no Proxy correspondente
    }
}
```

`evaluate(&self)` (contrato do trait) só consegue mutar campos de `self` através de mutabilidade
interior — mesmíssimo raciocínio que já vale hoje pra `command: Cell<f64>` no `Actuator`. A macro
geraria os campos "cache" como `Cell<f64>` por trás, com `compute(&self)` operando sobre eles via
`.get()`/`.set()` gerados automaticamente, ou (mais simples de gerar corretamente) usando um único
`RefCell<CompressorState>` interno pro bloco todo, com `compute(&mut self)` chamado através de
`.borrow_mut()`.

### 4.3 Veredito

**Candidato B (derive sobre struct) é o mais defensável** — usa o mecanismo de atributo-por-item que
a linguagem garante de verdade (helper attributes de derive), numa única macro/uma única passada,
sem depender de dois itens concordando por convenção de nome. Custa a ergonomia de "função pura"
(preciso de `&mut self`/mutabilidade interior gerada, não parâmetros → retorno) — mas ainda assim é
uma redução real: o `struct` inteiro (49 campos `Proxy`, no caso do `Reactor`) e o `impl
DynamicModel` inteiro deixam de existir escritos à mão; sobra só a lista de campos com atributo +
`compute()`, que é exatamente a física que já existe em `evaluate()` hoje, só sem os `.get()`/
`.set()` espalhados.

---

## 5. O que continua precisando existir em runtime, mudando nada

Nenhum dos quatro casos elimina peça nenhuma do que já existe — a macro só evita que o *usuário*
escreva a chamada:

- `StateRegistry` (`subscribe`/`resolve`/`commit`/`offer_*`/`need_*`) — inalterado, a macro só gera
  chamadas pra cá.
- `Proxy`/`ReadProxy`/`SensorHandle`/`ActuatorHandle` — inalterados, ainda o que carrega o valor de
  verdade.
- `actuator::model::Actuator`/`sensor::model::Sensor`/`controller::model::Controller` — inalterados;
  a macro só chama `::new()`.
- O erro de "`need` sem `offer` correspondente" continua só existindo em `resolve()`, em runtime —
  nenhuma macro consegue mover isso pra tempo de compilação, porque isso depende de saber, pro
  programa inteiro, quem mais oferece aquela chave — informação que só existe depois que todo mundo
  já rodou seu próprio `new()`, e macros não têm visão do programa inteiro (cada invocação só vê os
  próprios tokens). Resolver isso de verdade exigiria um mecanismo de registro cruzado
  (`inventory`/`linkme`/`ctor`) — o mesmo "item em aberto" que `CONTRIBUTING.md` Art. 6.2 §1 já
  registra pra um problema vizinho (auto-inscrição de `DynamicModel`). Fora de escopo aqui de
  propósito — ver resumo executivo.

## 6. Limitações reais, sem retoque

### 6.1 `proc-macro = true` é macro-only

Um crate marcado `proc-macro = true` no `Cargo.toml` não pode também exportar tipos/funções normais
— é uma regra dura do Cargo, sem exceção estável hoje. `monjolo` precisaria virar dois crates:
`monjolo` (o que já existe, runtime) e um novo `monjolo-macros` (só as quatro macros), com `monjolo`
dependendo de `monjolo-macros` e reexportando (`pub use monjolo_macros::{actuator, sensor,
dynamic_model, controller};`) — o mesmo padrão de `serde`/`serde_derive`,
`tokio`/`tokio-macros`. `tep-plant` continua só dependendo de `monjolo`.

### 6.2 Atributo em parâmetro de função (Candidato A) é frágil, não padrão

Já coberto em 4.1 — funciona, mas depende inteiramente da macro consumir o item antes do compilador
tentar validar os atributos internos; não é o mecanismo que a própria linguagem desenhou pra este
caso (esse é o helper-attribute de derive, usado no Candidato B).

### 6.3 Corpo vazio incomoda (Controller, Candidato A do DynamicModel)

`fn reactor_pressure_control() {}` (§3) ou uma função cujo corpo nunca roda de verdade porque a
macro descarta tudo (Candidato A, se o usuário só quisesse declarar campos sem física) são sintaxes
que *parecem* fazer algo e não fazem. Alternativa mais honesta pro Controller: atributo sobre
`struct` unitária, não `fn` — `#[controller(...)] struct ReactorPressureControl;` deixa mais claro
que não existe corpo de execução nenhum ali, só uma declaração.

### 6.4 Ferramentas

Erros de compilação dentro de código gerado apontam pro código gerado, não pro que o usuário
escreveu, a não ser que a macro seja cuidadosa preservando `Span` (via `syn`/`quote` — geralmente
bom, não perfeito). Autocomplete/go-to-definition do rust-analyzer dentro de regiões fortemente
geradas por macro varia de qualidade. Custo real, não hipotético — vale citar sem minimizar.

### 6.5 Testabilidade — o lado bom

Ponto positivo que vale destacar, não só limitação: pro Actuator e Sensor (Candidatos com função
livre sem captura), a macro pode manter a função pura original acessível pra teste direto, sem
StateRegistry nenhum envolvido — testar a física vira uma chamada de função comum. Hoje isso exige
`StateRegistry::shared()` + `resolve()` só pra chegar no número calculado (ver
`subsystems/compressor.rs` atual, `mod tests`). Pro DynamicModel via Candidato B, `compute()`
também fica testável sozinho — só que agora com um `Compressor` de verdade (com os campos `f64` já
setados manualmente em vez de vir de `Proxy`), não com `StateRegistry` nenhum.

---

## 7. Perguntas em aberto pra decidir antes de implementar

1. DynamicModel: Candidato A (função pura, correlação por convenção de nome entre duas macros) ou
   Candidato B (derive sobre struct, uma macro só, `compute(&mut self)` em vez de função pura)?
2. Controller: `fn` com corpo vazio ou `struct` unitária (§6.3)?
3. Vale a pena `monjolo-macros` já agora, ou isso espera até o dia em que a macro for implementada
   de verdade (esta investigação não exige o crate novo existir ainda)?
4. Escopo continua deliberadamente restrito a "reduzir a chamada por componente" — descoberta
   automática cross-crate (`inventory`/`linkme`/`ctor`) fica de fora, mesmo que a tentação de juntar
   os dois problemas apareça no meio da implementação?
