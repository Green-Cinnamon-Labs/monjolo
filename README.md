# monjolo

[![License: Apache-2.0](https://img.shields.io/github/license/Green-Cinnamon-Labs/monjolo)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)

**Monjolo is a deterministic runtime for continuous dynamic process simulation.**

Ele roda modelos dinâmicos como processos vivos: um loop de simulação que integra o estado no tempo (RK4), componentes trocando sinais por nome (`DynamicModel`/`StateRegistry`), blocos de sensor/atuador de primeira ordem, e uma fronteira de I/O (`IoImage`) pensada para expor esses sinais a protocolos industriais — hoje um adapter OPC-UA opcional (feature `opcua`), com espaço para outros no futuro. É genérico de propósito: não sabe nada de Tennessee Eastman, química, ou qualquer planta específica.

---

## O nome

Um monjolo é uma máquina hidráulica rústica de descascar/moer grãos, movida continuamente pela força da água — símbolo marcante da cultura caipira e do interior do Brasil, apesar de sua origem oriental. A metáfora é direta: este crate é o mecanismo que gira sozinho, tick após tick, movido pela integração numérica — e qualquer coisa que se queira "moer" (um reator, um pêndulo, uma rede de tanques) se encaixa nele sem que o mecanismo em si precise saber o que está processando.

---

## Origem

Este crate nasceu de dentro do [`tep-plant`](https://github.com/Green-Cinnamon-Labs/tep-plant), o simulador Rust do Tennessee Eastman Process (TEP) do laboratório — que por sua vez é um fork de [`camaramm/tennessee-eastman-profBraatz`](https://github.com/camaramm/tennessee-eastman-profBraatz), a implementação FORTRAN de referência do modelo de Downs & Vogel (1993).

Durante a refatoração do `tep-plant` para expor a planta via OPC-UA (ver `spec-tennessee-eastman`, issues #55/#57), ficou claro que boa parte do código não tinha nada a ver com Tennessee Eastman: gerenciamento de estado, integração numérica (RK4), dinâmica de primeira ordem de válvulas/atuadores, geração de distúrbios, carregamento de condição inicial, o ciclo de vida de threads (planta + adapter) — tudo isso era genérico, reaproveitável por qualquer planta simulada. Só a química, a termodinâmica e a topologia dos subsistemas (reator, separador, stripper, compressor) eram, de fato, específicas do TEP.

Separar essas duas coisas teve dois motivos:

- **Pedagógico**: outros alunos podem montar seus próprios modelos dinâmicos sobre o `monjolo` sem precisar entender ou reescrever a maquinaria de simulação — só implementam `DynamicModel` para o que for específico do seu problema.
- **Organizacional**: a separação de responsabilidades tornou o próprio `tep-plant` mais fácil de entender e evoluir, e deixou mais fácil enxergar onde cada melhoria (um novo integrador, um novo adapter, um novo tipo de sensor) realmente pertence.

---

## O que é / o que não é

**É:**
- Um jeito de compor modelos dinâmicos (`DynamicModel`) em árvore, cada um lendo/escrevendo seu próprio estado por nome semântico.
- Um integrador numérico (RK4) desacoplado de tudo — só soma vetores de estado a partir de uma closure `dynamics`.
- Blocos genéricos reaproveitáveis: atuador de 1ª ordem (`Valve`, `Agitator`), sensor com comportamento plugável (`Ideal`, `Noisy`, `Hysteresis`), canal de distúrbio cúbico C¹-contínuo.
- Um runtime supervisionado (`Simulation`) que roda a planta e, opcionalmente, um adapter de rede (hoje só OPC-UA) em threads separadas.

**Não é:**
- Um simulador do Tennessee Eastman — isso é o `tep-plant`, que consome este crate.
- Um framework de controle — não há noção de controlador/malha aqui; isso é responsabilidade de quem constrói o modelo (ou de um repositório supervisório, como o `tep-operator`).

---

## Conceitos centrais

### `DynamicModel` / `CompositeDynamicModel`

Interface central: `evaluate()` recalcula os valores/derivadas de um componente lendo/escrevendo via `Proxy`s que ele já guarda desde a inscrição — nunca por lookup de string no caminho quente. `CompositeDynamicModel` é o supertrait de quem orquestra outros: `add_dynamic()` só ordena a sequência de avaliação, cada componente-filho é quem declara seus próprios slots ao se inscrever no `StateRegistry`.

Componentes-folha (ex.: `Valve`) não implementam `CompositeDynamicModel` — tentar compô-los é erro de compilação, não de runtime.

### `StateRegistry`, `Proxy`, `ReadProxy`

Guarda dois buffers distintos:
- `EvaluationState` — cópia de trabalho de uma rodada de avaliação, pode conter valores hipotéticos (sub-passos do RK4). Endereçado por `Proxy`.
- `CurrentState` — o último estado confirmado. Endereçado só por `ReadProxy` (somente leitura, nunca hipotético), usado por `Sensor`.

`subscribe(offers, needs)` reserva slots e devolve `Proxy`s; posições são append-only e resolvidas **uma única vez** — depois disso, leitura/escrita é indexação direta em `Vec<Cell<f64>>`, sem hashing. `commit()` copia `EvaluationState → CurrentState` no fim de cada tick.

### `NumericalMethod` / `Integrator`

Enum fechado (hoje só `RK4`) — só o framework decide quais métodos existem, para não deixar `Simulation` aceitar um integrador arbitrário de fora. O `Integrator` não sabe nada de `Proxy`/`DynamicModel`: recebe um vetor de estado e uma closure `dynamics: &[f64] -> Vec<f64>`, devolve o próximo estado.

### Blocos genéricos

- **`actuator::dynamic`** — `Valve`/`Agitator`: atraso de 1ª ordem, `d(posição)/dt = (comando - posição) / τ`.
- **`sensor::model`** — `Sensor` lê `CurrentState` via `ReadProxy` e aplica um `SensorBehavior` plugável: `Ideal` (sem transformação), `Noisy` (ruído gaussiano), `Hysteresis` (banda morta).
- **`disturbance::cubic`** — `DisturbanceChannel`: polinômio cúbico por partes, regenerado continuamente, com continuidade C¹ (valor e derivada) nas junções — produz um sinal aleatório suave.
- **`snapshot`** — `Snapshot::from_file` achata um TOML qualquer em `"caminho.pontuado" -> f64`, sem saber o que cada chave significa; cada componente busca só as chaves que lhe interessam na sua própria construção.

### `IoImage`

Catálogo nomeado de sensores (leitura) e `CommandSink`s (escrita) — a fronteira externa mínima do framework, análoga a uma imagem de I/O de planta real. Não sabe nada de `DynamicModel`/RK4/`StateRegistry`; é o que um adapter de rede consome.

### `Simulation`

Builder até `run()` ser chamado (`set_model`, `set_adapter`, `add_sensor`, `add_actuator` só guardam definições). `run()` sobe até duas threads supervisionadas — "thread da planta" e "thread do adapter" — que só se comunicam por `SnapshotBus` (leitura) e `CommandQueue` (escrita), nunca pelo `StateRegistry` direto (que não é `Send`). Cada thread reporta exatamente um `ServiceEvent` (`Stopped`/`Failed`/`Panicked`, este último via `catch_unwind`) antes de encerrar; `run()` retorna assim que a primeira delas morrer.

### Adapters (feature `opcua`)

`AdapterConfig` também é um enum fechado — hoje só `OpcUa { endpoint }`. Sobe um servidor OPC-UA (via `async-opcua` + `tokio`, num runtime próprio criado dentro da thread do adapter) expondo cada sensor/atuador declarado como um node. `opcua`/`tokio` são dependências opcionais, atrás da feature `opcua`: o núcleo do framework não paga esse custo se não precisar de rede.

---

## Como usar

```rust
use monjolo::adapter::AdapterConfig;
use monjolo::dynamic_model::DynamicModel;
use monjolo::numerical_method::NumericalMethod;
use monjolo::simulation::Simulation;
use monjolo::state_registry::{Proxy, StateRegistry};

// Um DynamicModel mínimo: decaimento exponencial dv/dt = -v.
struct Decay {
    value: Proxy,
    derivative: Proxy,
}

impl Decay {
    fn new(registry: &mut StateRegistry) -> Self {
        let (offered, _) = registry.subscribe(&["decay.value", "decay.value.derivative"], &[]);
        offered[0].set(100.0);
        Self { value: offered[0].clone(), derivative: offered[1].clone() }
    }
}

impl DynamicModel for Decay {
    fn evaluate(&self) {
        self.derivative.set(-self.value.get());
    }

    fn state_keys(&self) -> Vec<String> {
        vec!["decay.value".to_string()]
    }
}

fn main() {
    let mut simulation = Simulation::new();
    simulation.set_model(Decay::new);
    simulation.set_numerical_method(NumericalMethod::RK4);
    simulation.set_adapter(AdapterConfig::OpcUa { endpoint: "opc.tcp://0.0.0.0:4840/demo/".into() });
    simulation.run().expect("run encerrou com erro");
}
```

Para um exemplo real e mais rico (composição de vários subsistemas, condição inicial via `Snapshot`, sensores declarados pelo próprio modelo), veja como o [`tep-plant`](https://github.com/Green-Cinnamon-Labs/tep-plant) (branch `composite`) implementa `TennesseeEastmanModel` sobre este crate — `src/model.rs` e `src/bin/tep_plant.rs`.

---

## Features do Cargo

| Feature | Padrão | O que ativa |
|---|---|---|
| *(nenhuma)* | — | Núcleo: `DynamicModel`, `StateRegistry`, `NumericalMethod`/RK4, atuador/sensor/distúrbio, `Snapshot`, `IoImage`. Só depende de `rand`/`rand_distr`/`toml`. |
| `opcua` | desligada | Adiciona `adapter::opcua` — sobe `async-opcua` + `tokio` (dependências pesadas demais para serem padrão de um framework de simulação genérico). |

```bash
cargo build                  # núcleo, sem rede
cargo build --features opcua # com o adapter OPC-UA
cargo test --features opcua  # roda toda a suíte de testes de unidade
```

---

## Quem consome isso hoje

| Repositório | Papel |
|---|---|
| [`tep-plant`](https://github.com/Green-Cinnamon-Labs/tep-plant) (branch `composite`) | Implementa `TennesseeEastmanModel` sobre `monjolo` — a química/termodinâmica do TEP compostas a partir de `Reactor`/`Separator`/`Stripper`/`Compressor`, cada um um `DynamicModel`. É o consumidor de referência e o motivo do crate existir. |
| [`spec-tennessee-eastman`](https://github.com/Green-Cinnamon-Labs/spec-tennessee-eastman) | Ponto de entrada do lab — decisões de arquitetura, experimentos e rastreamento de tarefas que atravessam todos os repositórios, incluindo este. |

`monjolo` hoje é consumido só via *path dependency* local (`../monjolo`) — ainda não foi publicado como dependência `git`/crates.io separada; ver comentário no `Cargo.toml` do `tep-plant`.

---

## Estado atual / limitações conhecidas

- Só existe um método numérico (`RK4`) e um adapter (`OpcUa`) — os dois são enums fechados por design, então adicionar um novo é uma mudança no próprio crate, não uma extensão de fora.
- Não há cancelamento cooperativo: se a thread da planta e a do adapter estão rodando e uma morre, a outra não é avisada — cabe a quem chamou `run()` decidir encerrar o processo.
- Sem testes de integração/exemplos separados ainda — a cobertura hoje é só `#[cfg(test)]` inline em cada módulo (12 testes no núcleo + feature `opcua`).
- Sem versionamento/publicação formal (`0.1.0`, dependência local só).

---

## Licença

O código deste repositório é licenciado sob [Apache-2.0](LICENSE). Dependências de terceiros mantêm suas próprias licenças — ver [NOTICE.md](NOTICE.md).

Para auditar as licenças de todo o grafo de dependências (inclusive transitivas):

```bash
cargo install cargo-deny
cargo deny check licenses
```

Vulnerabilidades de segurança: ver [SECURITY.md](SECURITY.md).

---

## Testes

```bash
cargo test --features opcua
```
