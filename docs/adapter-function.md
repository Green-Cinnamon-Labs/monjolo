# Adaptador de função — o `ModelFunction` servido por HTTP

`monjolo::adapter::function` (`monjolo/adapter/function.rs`, spec-tennessee-eastman#94) serve um `ModelFunction` por HTTP — o `StateRegistry` de um modelo, exposto pra quem quer avaliá-lo num ponto de fora do processo (um otimizador, por exemplo). Não sabe nada de TEP nem de otimização: só de chaves, valores e avaliar. O que a função avaliada garante está em [model-function.md](model-function.md).

Requer a feature `function` (`tiny_http` + `serde_json`).

## Rotas

- `GET /healthz` — `{"status":"ok","keys":N,"states":M}`.
- `GET /keys` — `{"keys":[{"key":"...","value":1.0,"state":true}, ...]}`: toda chave do registry, com o valor no ponto base e a marca de quais são estados integráveis.
- `POST /evaluate` — corpo `{"read":["k1","k2"],"points":[{"k3":1.0},{"k3":2.0}]}`; resposta `{"read":["k1","k2"],"values":[[..],[..]]}`, uma linha por ponto, na ordem de `read`. Cada ponto é o conjunto de chaves a escrever por cima do ponto base (`ModelFunction::evaluate`). Vários pontos por chamada, porque um gradiente por diferenças finitas pede dezenas de avaliações.

Valor não finito (NaN, infinito) não existe em JSON: sai como `null`.

## Erros

Toda resposta de erro tem o corpo `{"error":"..."}`.

- `400` — culpa de quem chamou: corpo ilegível, JSON malformado, falta de `read` ou `points`, chave que não existe, valor que não é número. Quando o problema é de um ponto, a mensagem diz qual (`ponto 2: ...`).
- `404` — rota desconhecida.
- `405` — método não suportado numa rota que existe.
- `500` — a própria avaliação do modelo entrou em pânico num ponto.

## Uma thread só

Síncrono e de uma thread só, de propósito: `ModelFunction` é `Rc`-based (`!Send`), então quem avalia tem que ser a mesma thread que montou o modelo. Um laço que recebe, avalia e responde nessa thread dispensa canal e runtime assíncrono; uma avaliação leva microssegundos.

## Interface

- **`FunctionServer::bind(address)`** — `address` no formato `host:porta`, ex.: `"127.0.0.1:8091"`. Porta `0` deixa o sistema escolher uma livre — ver `local_address()`.
- **`local_address()`** — o endereço em que o servidor ficou de fato, `host:porta`.
- **`serve(function)`** — atende requisições pra sempre, na thread de quem chama — a mesma que montou `function`.

## Testes

`tests/adapter_function.rs` sobe o servidor numa porta livre e fala com ele por HTTP de verdade. Só roda com a feature ligada: `cargo test --features function`.
