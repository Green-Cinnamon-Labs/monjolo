# Streams ao estilo Spring Boot — esboços, nada implementado

Esta pasta existe porque, olhando o código de `Separator::physical_state` (ver `docs/10-streams.md`), a reação foi "isso está tosco — eu queria declarar propriedade uma vez, tipo classe, e ter aquilo injetado, não redeclarar `#[need]`/`#[offer]` toda vez que um método precisa do mesmo dado." A referência dada foi Spring Boot (Java): uma classe declara do que precisa, um container resolve e injeta, e o resto do código só usa o campo — nunca pede a mesma coisa duas vezes. Os arquivos aqui são ESBOÇOS (pseudocódigo Rust, não compila, não foi testado) pra comparar formas concretas de trazer essa sensação pra este framework. Nenhum código real foi tocado.

## O que já existe aqui que já é "Spring-like" (pra não redescobrir a roda)

Vale notar antes de tudo: este framework (`monjolo`) já tem METADE da ideia do Spring, só que ninguém tinha nomeado assim. `#[actuator(...)]`, `#[sensor(...)]`, `#[controller(...)]`, `#[dynamic_model]` já fazem o que `@Component`/`@Service` fazem no Spring — uma classe anotada se auto-registra num catálogo central (`inventory::submit!`, coletado por `attach_discovered_components`, que é literalmente o papel do `ApplicationContext` do Spring: escanear tudo que foi marcado e montar o grafo de objetos no boot). Isso já existe, já funciona, e nenhum dos esboços abaixo muda essa parte.

O que NÃO existe, e é o que está incomodando: o equivalente de `@Autowired` — pegar uma dependência UMA VEZ (como um campo da classe) e usá-la à vontade dali pra frente. Hoje, `#[need(key = "...")]` só existe colado num MÉTODO (a metade "tasks" do framework), então cada método que quer o mesmo dado tem que pedir de novo — não existe "pedir uma vez, no construtor, guardar como propriedade."

## Os esboços

- **`alt1-campo-injetado.md`** — a Stream vira um CAMPO do struct (declarado uma vez, tipo `@Autowired`), qualquer método do `impl` usa `self.stream.composicao()` livremente. Resolve a duplicação, mas a Stream continua sendo "dona" de quem a oferece (mesma assimetria oferece/precisa de hoje).

- **`alt2-stream-como-servico.md`** — a Stream vira ELA MESMA uma classe auto-registrada (como `#[actuator]` já é hoje), independente das duas unidades que ela conecta — cada lado (quem escreve, quem lê) recebe uma referência a ela via injeção (usa o mecanismo do esboço 1 por baixo). Mais parecido com um `@Service` compartilhado que duas outras classes injetam.

- **`alt3-configuracao-explicita.md`** — nota curta, não um esboço completo: o equivalente ao Spring `@Configuration`/`@Bean` (fiação explícita, escrita à mão, em vez de auto-descoberta por `inventory`). Este projeto já ABANDONOU esse estilo uma vez (existia um `build_tep()` manual, removido em favor de auto-descoberta total) — incluído só por completude, não é recomendação.

Leia os três e diz qual direção (ou mistura) faz mais sentido — só depois disso vira uma proposta de verdade, com plano de mudança na macro.
