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
    fn name(&self) -> &'static str { "unnamed" }

    fn evaluate(&self);
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
