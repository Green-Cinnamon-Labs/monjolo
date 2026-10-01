# Alternativa 3 — Configuração explícita (`@Configuration`/`@Bean`), nota curta

Não é um esboço completo como os outros dois — é só pra registrar que essa terceira opção existe, do jeito que o Spring também oferece duas formas de montar o container: escaneamento automático por anotação (`@ComponentScan`, o que as Alternativas 1/2 fazem, espelhando o que `#[actuator]`/`#[sensor]`/`#[dynamic_model]` já fazem hoje via `inventory`) OU uma classe de configuração que constrói e conecta tudo à mão (`@Configuration` + métodos `@Bean`).

Este projeto já teve o equivalente disso — um `build_tep()` que construía cada unidade manualmente — e ABANDONOU esse estilo de propósito, migrando pra descoberta automática total via `inventory` (ver `lib.rs`: "não existe mais `model.rs`/`build_tep()`"). Reintroduzir fiação explícita pra Streams especificamente significaria ter DOIS estilos de wiring coexistindo no mesmo projeto (automático pra unidades, manual pra streams) — provavelmente inconsistente, não recomendado sem um motivo concreto que as Alternativas 1/2 não resolvam.

Incluído só por completude — nenhum esboço de código aqui.
