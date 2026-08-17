/** Marcador de contrato — Controller ainda não tem design fechado (nenhum consumidor real existe
ainda pra decidir `step()`/frequência/scheduler contra). Existe aqui só pra reservar o nome/lugar, ao
lado de `Sensor`/`Actuator`, como o terceiro objeto que a física simulada usa pra interagir com o
mundo de fora.

A implementação concreta genérica (`model`) mora aqui mesmo, mas só cobre a metade que já tem design
fechado: declarar quais `Sensor`s/`Actuator`s nomeados um controller precisa, resolvidos pelo mesmo
ciclo declare → register → resolve → inject do `StateRegistry` (Art. 6.3 §1 do CONTRIBUTING). A
lógica de controle em si (ler, decidir, escrever) continua em aberto — decisão pra quando houver um
Controller de verdade pra testar contra, não agora.
*/
pub trait Controller {}

pub mod model;
