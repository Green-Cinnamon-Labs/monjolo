/* monjolo/lib.rs */

/* O código gerado por #[actuator(...)] (e futuras macros irmãs) usa caminhos absolutos
`::monjolo::...` — precisa, porque quem expande normalmente é um crate de fora (ex.: tep-plant), sem
nenhum `use` do usuário garantido. Isso só quebra quando o PRÓPRIO monjolo usa sua macro em seus
próprios testes (component.rs): sem este alias, `::monjolo::` não existe de dentro da compilação do
próprio monjolo. Truque padrão pra crates que testam sua própria macro-geração consigo mesmos.
*/
extern crate self as monjolo;

pub mod actuator;
pub mod adapter;
pub mod component;
pub mod controller;
pub mod disturbance;
pub mod dynamic_model;
pub mod numerical_method;
pub mod sensor;
pub mod simulation;
pub mod snapshot;
pub mod state_registry;

/* `actuator`/`sensor`/`controller`/`dynamic_model` (macro) e os módulos de mesmo nome acima não
colidem — macro de atributo vive num namespace separado de módulo/tipo/valor; mesmo truque que o
serde usa pra `Serialize` (trait) e `Serialize` (derive) coexistirem sob o mesmo nome.
`monjolo-macros` é crate-only-macro (ver seu Cargo.toml); quem usa `monjolo` nunca precisa saber
que ele existe.
*/
pub use monjolo_macros::{actuator, controller, dynamic_model, sensor};

pub use component::{attach_discovered_components, ComponentDescriptor, ComponentKind};

/* Reexportado por inteiro (não só `submit!`/`collect!`/`iter`): o código que `#[actuator(...)]`
gera vive dentro de quem USA a macro (ex.: tep-plant), então referencia `::monjolo::inventory::...`
— sem isto, cada crate que usasse a macro precisaria declarar `inventory` no próprio Cargo.toml só
pra macro-código gerado compilar, quebrando a promessa de "quem usa monjolo não sabe que isso existe
por baixo".
*/
pub use inventory;
