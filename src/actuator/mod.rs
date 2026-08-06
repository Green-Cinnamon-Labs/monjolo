/** Contrato mínimo de escrita — a contraparte de `Sensor` (`sensor/mod.rs`).
Quem implementa isso recebe comandos de fora; `monjolo` não sabe nada sobre
o que acontece com o valor escrito (pode virar `command` de um `Valve` com
defasagem de 1ª ordem, pode ser ignorado, pode alimentar outra coisa
qualquer — decisão de quem implementa).

Um `DynamicModel` pode perfeitamente também implementar `Actuator` — não há
conflito: são dois traits distintos, um componente físico (`Valve`,
`Agitator`) pode ser as duas coisas ao mesmo tempo (ver CONTRIBUTING.md,
discussão sobre Valve/Agitator serem atuadores de verdade, não só uma
caixa de correio separada).
*/
pub trait Actuator {
    fn write(&self, value: f64);
}
