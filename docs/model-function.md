# ModelFunction — o modelo como função, fora do laço de simulação

`monjolo::model_function::ModelFunction` (`monjolo/model_function.rs`, spec-tennessee-eastman#91/#92) trata o modelo como uma FUNÇÃO: escrever valores em chaves do `StateRegistry`, avaliar a árvore e ler outras chaves de volta. É o que um otimizador precisa — derivadas e medições num ponto qualquer, sem tempo, sem integração e sem thread.

## O que pode ser lido e escrito

Toda chave oferecida no registry pode ser lida. Escrever só tem efeito numa chave que nenhum componente recalcula durante `evaluate()` (um estado integrável, o liga/desliga de um distúrbio): o resto é sobrescrito pela própria avaliação.

## O ponto base

Cada avaliação parte do PONTO BASE — o conteúdo inteiro do `EvaluationState` logo depois da montagem, com os estados semeados pelo `Snapshot` — e só então aplica as escritas pedidas. É isso que faz o resultado depender só do que foi pedido na própria chamada: sem essa restauração, um estado escrito numa chamada continuaria valendo na seguinte.

## O que a restauração não cobre

A restauração não cobre o que um componente guarda FORA do registry (um `Cell` próprio). Por isso a montagem também recusa um modelo em que avaliar duas vezes seguidas o mesmo ponto muda algum valor — sinal de que alguma chave é lida antes de ser recalculada, ou de que algo acumula entre chamadas. Isso não substitui o teste A, B, A de quem usa: um componente pode repetir o mesmo valor no mesmo ponto e ainda assim carregar memória de outro ponto.

## Quem fica de fora

Sem `Simulation` e sem `commit()`: `Sensor` (que lê `CurrentState`) e `Controller` (que lê `Sensor`) não participam de `discovered()`.

## Interface

- **`ModelFunction::new(config, build)`** — monta a função a partir de um modelo construído à mão. `build` recebe o registry e o `Snapshot` de config, mesma assinatura da fábrica de `Simulation::set_model()`.
- **`ModelFunction::discovered(config)`** — monta a função só com o que `inventory` descobre (mesma descoberta de `Simulation`), deixando de fora `Controller` e `Sensor`: quem chama escreve direto o que um controlador comandaria, e lê as grandezas cruas do registry, sem o ruído que só existe dentro de um `Sensor`.
- **`keys()`** — todas as chaves do registry, em ordem alfabética.
- **`state_keys()`** — as chaves que o `Integrator` avançaria. Cada `K` aqui tem uma `K.derivative` em `keys()`.
- **`base(key)`** — valor de uma chave no ponto base. `None` se a chave não existe.
- **`evaluate(writes, reads)`** — avalia o modelo no ponto base com `writes` aplicado por cima, e devolve o valor de cada chave de `reads`, na mesma ordem. `Err` se alguma chave não existir — nesse caso nada é avaliado.

Os dois construtores devolvem `Err` quando um estado declarado em `state_keys()` não tem a chave `.derivative` oferecida por ninguém, ou quando o modelo não passa pela checagem das duas avaliações seguidas.

## Testes

`tests/model_function.rs` cobre cada regra acima com modelos de brinquedo, incluindo um que passa pela checagem da montagem e só é denunciado pelo teste A, B, A.
