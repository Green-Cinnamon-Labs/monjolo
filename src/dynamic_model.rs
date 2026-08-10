/* core/dynamic_model.rs */

/** Interface central do sistema de modelagem dinâmica. O layout do estado (antes
get_state_template()/StateTemplate) e a persistência (antes set_state()) saíram do DynamicModel —
quem declara slots é a inscrição de cada DynamicModel em um StateRegistry singleton
(state_registry.rs), via subscribe(), que devolve Proxys. Um Proxy já carrega o buffer compartilhado
junto com o índice — por isso evaluate() não recebe `state` nem `EvaluationState` nenhum: cada
componente já tem, desde a inscrição, todos os Proxys de que precisa (seu próprio estado, suas
derivadas, os valores de outros componentes) guardados como campos, e só usa
`proxy.get()`/`proxy.set(valor)`. "Ter uma dinâmica a ser avaliada" não significa "produzir uma
derivada": um DynamicModel sem derivada nenhuma (ex.: um agregador puro) ainda é afetado pela
integração, porque os valores que ele lê mudam a cada rodada — só não é ele quem o Integrator soma
no vetor de estado.
*/
use std::rc::Rc;

pub trait DynamicModel {
    /** `&str`, não `&'static str`: precisa poder apontar pra um nome guardado dentro de `self`
    (ex.: `Composite::named()`), não só um literal fixo no código.
    */
    fn name(&self) -> &str {
        "unnamed"
    }

    fn evaluate(&self);

    /** Chaves de estado que esse modelo declara como integráveis pelo `Integrator` (seção 8.3/9.3
    do plano). Cada chave `K` aqui precisa ter sido ofertada em `subscribe()` junto com sua
    contraparte `"K.derivative"` — é essa segunda chave que o `Integrator` lê pra saber quanto `K`
    muda por tempo (ex.: `Valve` oferece `"valve.feed_a.position"` +
    `"valve.feed_a.position.derivative"` e declara só a primeira aqui).

    Vazio por padrão: a maioria dos `DynamicModel` (ex.: `Reactor`) não tem derivada própria pra
    integrar — quem sabe disso é sempre quem monta o composto, nunca o componente-folha por si.
    Chamado por `Simulation::set_model()`, com o tipo ainda concreto, antes de virar `Box<dyn
    DynamicModel>`.
    */
    fn state_keys(&self) -> Vec<String> {
        Vec::new()
    }
}

/** Contrato de Composição: CompositeDynamicModel estende DynamicModel (supertrait) — implementar
esse trait exige implementar o outro também. Só os DynamicModel que são nós compostos (ex.:
TennesseeEastmanModel) implementam isso. Componentes-folha (Valve, Agitator) não implementam —
tentar compô-los vira erro de compilação, não de runtime.

`add_dynamic` não declara slots nem funde template nenhum — quem declara slots é a inscrição de cada
DynamicModel direto no StateRegistry (fora deste trait). O papel de `add_dynamic` é só ordenar:
adiciona o componente à sequência de avaliação do composto, na ordem em que foi inserido.
`models`/`models_mut` são os únicos métodos que cada composto concreto precisa escrever — getters
triviais pro próprio `Vec<Box<dyn DynamicModel>>`. Com eles, `evaluate_children()` já dá conta de
rodar todo mundo na ordem certa — o `impl DynamicModel` do composto só precisa chamar isso.
*/
pub trait CompositeDynamicModel: DynamicModel {
    fn models(&self) -> &[Box<dyn DynamicModel>];
    fn models_mut(&mut self) -> &mut Vec<Box<dyn DynamicModel>>;

    fn add_dynamic(&mut self, component: Box<dyn DynamicModel>) {
        self.models_mut().push(component);
    }

    fn evaluate_children(&self) {
        for model in self.models() {
            model.evaluate();
        }
    }
}

/** Implementação genérica e pronta de `CompositeDynamicModel` — pra quem não precisa de nada além
de "guardar filhos e avaliar todos, na ordem em que foram adicionados". Cobre o caso comum sem
exigir declarar `struct` + `Vec` + os dois getters + `impl DynamicModel`/`CompositeDynamicModel`
toda vez (ver `CONTRIBUTING.md`, Art. 2.5 — cogitado antes e descartado por só existir um composto
no sistema; deixou de valer com um segundo caso real).

O que pertence a qualquer composto (guardar filhos, avaliar em ordem, ter um nome pros logs) mora
aqui. O que é específico de cada planta (que componentes exatamente compõem ela, com que parâmetros)
fica de fora — normalmente uma função livre em quem monta a planta, devolvendo um `Composite` já
pronto, sem precisar declarar tipo nenhum próprio.
*/
pub struct Composite {
    models: Vec<Box<dyn DynamicModel>>,
    name: String,
}

impl Default for Composite {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            name: "unnamed".to_string(),
        }
    }
}

impl Composite {
    pub fn new() -> Self {
        Self::default()
    }

    /** Define o nome exibido nos logs (`eprintln!` da `Simulation`, etc.) — sem chamar isso, fica
    `"unnamed"`. Encadeável: `Composite::new().named("X")`.
    */
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl DynamicModel for Composite {
    fn name(&self) -> &str {
        &self.name
    }

    fn evaluate(&self) {
        self.evaluate_children();
    }
}

impl CompositeDynamicModel for Composite {
    fn models(&self) -> &[Box<dyn DynamicModel>] {
        &self.models
    }

    fn models_mut(&mut self) -> &mut Vec<Box<dyn DynamicModel>> {
        &mut self.models
    }
}

/** Deixa um `Rc<T>` ser `add_dynamic`'d como qualquer outro `DynamicModel` — o caso que motiva isso
é um `Actuator` (ou qualquer outro componente) que precisa ser a mesma instância tanto dentro do
composto (`Box<dyn DynamicModel>`, participa de `evaluate()`) quanto fora dele, catalogado por nome
(`StateRegistry::offer_actuator`, `Rc<dyn Actuator>`) — sem essa impl, as duas exigiriam cópias
separadas, e um `Controller` que escreve na cópia catalogada nunca afetaria a física de verdade.
`Rc<T>`, não `Arc<T>`: os componentes concretos deste framework já são `!Send` (`Proxy` é
`Rc`-based), então `Arc` prometeria uma travessia de thread que não existe.
*/
impl<T: DynamicModel + ?Sized> DynamicModel for Rc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn evaluate(&self) {
        (**self).evaluate();
    }

    fn state_keys(&self) -> Vec<String> {
        (**self).state_keys()
    }
}
