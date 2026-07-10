// core/dynamic_model.rs

/**Interface central do sistema de modelagem dinâmica.
 O layout do estado (antes get_state_template()/StateTemplate) e a
 persistência (antes set_state()) saíram do DynamicModel — quem declara
 slots é a inscrição de cada DynamicModel em um StateRegistry singleton
 (state_registry.rs), via subscribe(), que devolve Proxys. Um Proxy já
 carrega o buffer compartilhado junto com o índice — por isso evaluate()
 não recebe `state` nem `EvaluationState` nenhum: cada componente já tem,
 desde a inscrição, todos os Proxys de que precisa (seu próprio estado,
 suas derivadas, os valores de outros componentes) guardados como campos, e
 só usa `proxy.get()`/`proxy.set(valor)`. "Ter uma dinâmica a ser avaliada"
 não significa "produzir uma derivada": um DynamicModel sem derivada
 nenhuma (ex.: um agregador puro) ainda é afetado pela integração, porque
 os valores que ele lê mudam a cada rodada — só não é ele quem o Integrator
 soma no vetor de estado.
*/
pub trait DynamicModel {
    fn name(&self) -> &'static str {
        "unnamed"
    }

    fn evaluate(&self);

    /** Sinais que esse modelo declara como observáveis — pares (nome de
    exposição, chave do `StateRegistry`). Vazio por padrão: a maioria dos
    `DynamicModel` (folhas como `Reactor`/`Valve`) não declara nada, só quem
    orquestra (ex.: `TennesseeEastmanModel`) sabe quais dos seus próprios
    slots fazem sentido expor pra fora — a mesma relação que `add_dynamic`
    tem com composição, `sensors` tem com observabilidade (drawio,
    aba "arquitetura": "TennesseeEastmanModel --DECLARA--> Sensor").

    Chamado por `Simulation::set_model()` uma única vez, com o modelo ainda
    no tipo concreto (antes de virar `Box<dyn DynamicModel>`) — por isso o
    `Simulation` nem precisa desse método pra tipos que não o sobrescrevem,
    o default vazio já resolve.
    */
    fn sensors(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /** Chaves de estado que esse modelo declara como integráveis pelo
    `Integrator` (seção 8.3/9.3 do plano). Cada chave `K` aqui precisa ter
    sido ofertada em `subscribe()` junto com sua contraparte `"K.derivative"`
    — é essa segunda chave que o `Integrator` lê pra saber quanto `K` muda
    por tempo (ex.: `Valve` oferece `"valve.feed_a.position"` +
    `"valve.feed_a.position.derivative"` e declara só a primeira aqui).

    Vazio por padrão, mesmo raciocínio de `sensors()`: a maioria dos
    `DynamicModel` (ex.: `Reactor`) não tem derivada própria pra integrar —
    quem sabe disso é sempre quem monta o composto, nunca o componente-folha
    por si. Chamado por `Simulation::set_model()` junto com `sensors()`, no
    mesmo momento (tipo ainda concreto, antes de virar `Box<dyn DynamicModel>`).
    */
    fn state_keys(&self) -> Vec<String> {
        Vec::new()
    }
}

/**Contrato de Composição: CompositeDynamicModel estende DynamicModel
 (supertrait) — implementar esse trait exige implementar o outro também.
 Só os DynamicModel que são nós compostos (ex.: TennesseeEastmanModel)
 implementam isso. Componentes-folha (Valve, Agitator) não implementam —
 tentar compô-los vira erro de compilação, não de runtime.

 `add_dynamic` não declara slots nem funde template nenhum — quem declara
 slots é a inscrição de cada DynamicModel direto no StateRegistry (fora
 deste trait). O papel de `add_dynamic` é só ordenar: adiciona o
 componente à sequência de avaliação do composto, na ordem em que foi
 inserido. `models`/`models_mut` são os únicos métodos que cada composto
 concreto precisa escrever — getters triviais pro próprio
 `Vec<Box<dyn DynamicModel>>`. Com eles, `evaluate_children()` já dá conta
 de rodar todo mundo na ordem certa — o `impl DynamicModel` do composto só
 precisa chamar isso.
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
