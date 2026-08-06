/** Marcador de contrato — Controller ainda não tem design fechado (nenhum
consumidor real existe ainda pra desenhar contra). Existe aqui só pra
reservar o nome/lugar, ao lado de `Sensor`/`Actuator`, como o terceiro
objeto que a física simulada usa pra interagir com o mundo de fora.

Quando um design real aparecer, provavelmente combina os dois outros
traits (lê via algo tipo `Sensor`, escreve via algo tipo `Actuator`) — mas
isso é decisão pra quando houver um Controller de verdade pra testar
contra, não agora.
*/
pub trait Controller {}
