/** monjolo/component.rs

A "ficha" que cada macro (`#[actuator(...)]` e, no futuro, `#[sensor(...)]`/`#[controller(...)]`/
`#[dynamic_model]`) emite escondida, via `inventory::submit!`, pra que `Simulation` monte a árvore
de avaliação sem que quem monta a planta precise listar manualmente cada componente declarado dessa
forma — `mod actuators; mod feed_d;` etc. ainda precisam existir (`inventory` não faz descoberta de
sistema de arquivos), mas nenhuma chamada `add_dynamic`/`offer_*` manual é necessária: a própria
declaração anotada já é suficiente pro componente se anunciar ao runtime.

`construct` já FAZ o registro no `StateRegistry` — chama o `new()` que a macro gerou, que por sua
vez já chama `subscribe()`/`offer_*` por dentro (nenhuma responsabilidade nova; só uma indireção pra
poder chamar isso de dentro de código genérico, sem conhecer o tipo concreto). É chamado exatamente
UMA vez, no bootstrap (`Simulation::run()`) — o resultado é a mesma instância que vive pelo resto
da simulação, nunca reconstruída por tick; `fn`, não closure com estado, é só o mecanismo que Rust
exige pra guardar "como construir X" num `static` sem conhecer X de antemão.

Devolve `Some(...)` quando o componente também precisa ser avaliado a cada tick (implementa
`DynamicModel`); `None` quando o componente só se cataloga, sem entrar na árvore de avaliação
(`Sensor` — leitura/transformação em tempo de transação, nunca `add_dynamic`'d).

`&Snapshot`: mesmo mecanismo de condição inicial que `Reactor::new(registry, initial)` já usa à
mão — em vez de cada componente ler um arquivo sozinho ("as classes não devem ler arquivos
diretamente"), `Simulation` carrega o `Snapshot` de config UMA vez no bootstrap e passa a mesma
referência pra todo `construct()`; `#[dynamic_model]` usa isso pra semear campos `#[config(...)]`
(ver monjolo-macros/dynamic_model.rs). `#[actuator]`/`#[sensor]`/`#[controller]` recebem o parâmetro
mas ainda não o usam — nenhum dos três tem campo `#[config(...)]` hoje.
*/
use std::collections::HashMap;

use crate::dynamic_model::{Composite, CompositeDynamicModel, DynamicModel};
use crate::snapshot::Snapshot;
use crate::state_registry::StateRegistry;

/** A ordem de avaliação é uma invariante da arquitetura, não uma escolha por componente — três
fases fixas, sempre nesta ordem:

(A) `Dynamic` — DynamicModel "puro" (nem Actuator, nem Controller). `inventory::iter` sozinho não
garante ordem nenhuma ENTRE auto-descobertos da mesma fase — quando isso importa de verdade (ex.:
Separator precisa de `reactor.temperature` do MESMO tick, não do anterior), `#[dynamic_model(after
= [...])]` declara a dependência por nome (`ComponentDescriptor::name`, ver `sort_by_dependency`
abaixo), e `attach_discovered_components` ordena a fase (A) topologicamente antes de anexar. Sem
`after`, a ordem entre auto-descobertos continua não-garantida (ok pra quem é fisicamente
independente).

**Cadeia única, de propósito, não o grafo mínimo de dependência real.** `sort_by_dependency`
garante transitividade de graça numa cadeia PURA (`C after B`, `B after A` ⇒ `C` acaba depois de
`A` também, sem precisar listar `A`), mas NÃO garante nada contra um IRMÃO não listado (dois nós
que dependem do mesmo pai, sem depender um do outro — `sort_by_dependency_does_not_guarantee_
order_against_an_unlisted_sibling`, no módulo de teste aqui embaixo, prova isso com um caso
adversário). Hoje, `evaluate_children()` (`dynamic_model.rs`) roda sequencial, uma "Thread da
planta" só, sem paralelismo real possível (`Proxy`/`Composite` são `Rc`-based, `!Send`) — como não
existe "ao mesmo tempo" de verdade, não há vantagem em modelar o grafo de dependência MÍNIMO
(irmãos genuinamente independentes, ex. Stripper/Compressor, que só compartilham Separator como
pai): é mais simples, e igualmente correto, encadear TUDO da fase (A) numa única ordem total
artificial (cada um só lista o vizinho imediato) e deixar a transitividade de cadeia pura cobrir o
resto. **Quando isso deixar de ser verdade** (avaliação concorrente de verdade, exigindo redesenho
pra `Arc`/thread-safety) — os `after` hoje encadeados por conveniência (não por dependência real de
dado) precisam ser revisitados: só aí faz sentido voltar a expressar o grafo mínimo, pra permitir
que irmãos genuinamente independentes rodem em paralelo.

(B) `Actuator` — sempre depois de TODOS os (A). Ordem relativa entre atuadores não importa
fisicamente: cada um só lê o próprio comando/estado, não depende de outro atuador — `after` não
existe pra esta fase.

(C) `Controller` — sempre depois de TODOS os (B), pelo mesmo motivo (lê Sensor/Actuator já
avaliados nesta rodada, escreve comando pro próximo tick) — `after` também não existe aqui.

`Sensor` não é uma fase de avaliação — descritores de Sensor sempre devolvem `None` em `construct`,
não entram em `root`, mas `construct()` ainda roda (é o que cataloga a instância).
*/
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    Dynamic,
    Actuator,
    Controller,
    Sensor,
}

pub struct ComponentDescriptor {
    /* Usado pra diagnóstico E, agora, como chave do sort topológico dentro da fase (A) — nunca é
    chave de busca no StateRegistry (isso é `registry.actuator("...")` etc., independente disto).
    */
    pub name: &'static str,
    pub kind: ComponentKind,
    /** Nomes (`ComponentDescriptor::name`, não chave de StateRegistry) de outros componentes da
    MESMA fase que precisam ser construídos/anexados antes deste. Hoje é usado de duas formas: (1)
    desempate entre candidatos igualmente prontos no sort por `needs`/`offers` (ver
    `sort_phase_a` abaixo) — o caso comum antes de `needs`/`offers` existirem, ainda válido pra
    quem não os declara; (2) ordem estrutural sem chave de dado nenhuma por trás — é assim que
    `#[monjolo::tasks]` garante que a unidade dona já foi construída (e portanto já chamou
    `offer_instance`) antes de qualquer tarefa dela rodar `construct()`, injetando `after =
    [nome_da_struct]` automaticamente. Vazio (`&[]`) pros kinds `Actuator`/`Controller`/`Sensor`.
    */
    pub after: &'static [&'static str],
    /** Chaves de `StateRegistry` que este componente PRECISA que já estejam publicadas por outro
    componente da MESMA fase antes de rodar — junto com `offers`, é o que permite `sort_phase_a`
    montar a ordem automaticamente a partir do GRAFO real de dependência (needs↔offers casando por
    chave), em vez de exigir uma cadeia `after` hand-escrita cobrindo cada vizinho. Vazio (`&[]`)
    por padrão — `#[actuator]`/`#[sensor]`/`#[controller]` não populam isso hoje.
    */
    pub needs: &'static [&'static str],
    /** Chaves que este componente PUBLICA nesta fase — o lado "oferece" do mesmo grafo. Duas
    chaves iguais ofertadas por dois componentes DIFERENTES da fase (A) é erro de programação
    (`StateRegistry::subscribe` sobrescreveria silenciosamente o índice de uma delas) — `sort_phase_a`
    detecta isso e entra em pânico no bootstrap, antes do primeiro `evaluate()`.
    */
    pub offers: &'static [&'static str],
    pub construct: fn(&mut StateRegistry, &Snapshot) -> Option<Box<dyn DynamicModel>>,
}

inventory::collect!(ComponentDescriptor);

/** Varre `inventory::iter::<ComponentDescriptor>()` e anexa a `root` cada componente descoberto
que devolve `Some` de `construct`, respeitando a fase fixa (A/B/C — ver `ComponentKind`).
`Simulation::set_model()` chama isso depois de `root` já ter o modelo montado à mão como primeiro
filho (fase A, junto dos subsistemas físicos); extraído aqui, e não deixado inline em
`simulation.rs`, pra dar pra testar a descoberta sem precisar subir a "Thread da planta" inteira.

`pub`, não `pub(crate)`: quem monta a planta (ex. `tep-plant::model::build_tep()`) só constrói mais
os subsistemas que ainda não migraram pra macro — testar que "a cadeia inteira funciona" desse lado
exige rodar a MESMA descoberta que `Simulation::run()` roda, sem precisar subir a Thread da planta
só pra isso (ver `component::tests` aqui mesmo, que já usa isto diretamente).
*/
pub fn attach_discovered_components(root: &mut Composite, registry: &mut StateRegistry, config: &Snapshot) {
    let mut phase_a: Vec<&'static ComponentDescriptor> = Vec::new();
    let mut phase_b: Vec<&'static ComponentDescriptor> = Vec::new();
    let mut phase_c: Vec<&'static ComponentDescriptor> = Vec::new();
    let mut sensors: Vec<&'static ComponentDescriptor> = Vec::new();

    for descriptor in inventory::iter::<ComponentDescriptor> {
        match descriptor.kind {
            ComponentKind::Dynamic => phase_a.push(descriptor),
            ComponentKind::Actuator => phase_b.push(descriptor),
            ComponentKind::Controller => phase_c.push(descriptor),
            ComponentKind::Sensor => sensors.push(descriptor),
        }
    }

    /* Sensor nunca entra em `root` (não é DynamicModel, sem fase, sem ordem entre si) — mas
    `construct()` ainda PRECISA rodar: é o que de fato cria e cataloga a instância
    (`offer_sensor()`, dentro do `new()` gerado). Sem chamar isto, o sensor nunca existiria.
    */
    for descriptor in &sensors {
        let instance = (descriptor.construct)(registry, config);
        debug_assert!(instance.is_none(), "descritor de Sensor ({}) devolveu Some — Sensor não é DynamicModel", descriptor.name);
    }

    for descriptor in sort_phase_a(phase_a).into_iter().chain(phase_b).chain(phase_c) {
        if let Some(instance) = (descriptor.construct)(registry, config) {
            root.add_dynamic(instance);
        }
    }
}

/** Sort topológico (Kahn, O(n²) — a fase (A) tem poucas dezenas de componentes na prática, não
precisa de nada mais esperto) sobre o GRAFO REAL de dependência: um candidato está pronto quando
(a) todo nome em `after` já foi ordenado ou não corresponde a nenhum descritor desta fase (ordem
estrutural, sem chave de dado — ver comentário de `ComponentDescriptor::after`), E (b) toda chave em
`needs` ou não é ofertada por ninguém nesta fase (satisfeita de fora — um valor de Actuator/Sensor,
ou um campo ainda não migrado pra `#[monjolo::tasks]`) ou já foi ordenado quem a oferece. Isso
substitui a cadeia `after` manual cobrindo cada vizinho por um grafo inferido automaticamente de
`needs`/`offers` — necessário a partir do momento em que unidades passam a ter várias tarefas
próprias com dependência cruzada real entre si (ex.: `Reactor::outlet_flow` precisa de
`separator.pressure`, `Separator::pressure` precisa de `reactor.temperature` — dois NÓS diferentes,
sem ciclo, impossível de expressar com `after` no grão de struct inteira).

Ao contrário do sort anterior (que nunca falhava — o que sobrasse sem progresso era despejado na
ordem em que apareceu), este ENTRA EM PÂNICO se: (1) duas chaves iguais são ofertadas por
descritores diferentes desta fase (bug de programação — `StateRegistry::subscribe` sobrescreveria
um índice silenciosamente); (2) sobra algum descritor sem conseguir progresso — um ciclo real, que
`StateRegistry::resolve()` jamais detectaria sozinho (ele só checa "existe ALGUM ofertante", nunca
"a ordem é executável"). As duas condições só podem acontecer por erro de quem declara
`needs`/`offers`/`after` a mão ou por um bug na geração da macro — nunca por dado de usuário em
runtime —, por isso pânico no bootstrap (antes do primeiro `evaluate()`) é apropriado, não um
`Result` que a chamada precisaria propagar.
*/
fn sort_phase_a(mut remaining: Vec<&'static ComponentDescriptor>) -> Vec<&'static ComponentDescriptor> {
    let mut offered_by: HashMap<&'static str, &'static ComponentDescriptor> = HashMap::new();
    for candidate in &remaining {
        for &key in candidate.offers {
            if let Some(previous) = offered_by.insert(key, candidate) {
                panic!(
                    "chave '{key}' ofertada por dois componentes da fase (A): '{}' e '{}' — \
                    StateRegistry::subscribe() sobrescreveria o índice de um dos dois silenciosamente",
                    previous.name, candidate.name,
                );
            }
        }
    }

    let is_settled = |name: &str, sorted: &[&'static ComponentDescriptor], remaining: &[&'static ComponentDescriptor]| {
        sorted.iter().any(|s| s.name == name) || !remaining.iter().any(|r| r.name == name)
    };

    let mut sorted: Vec<&'static ComponentDescriptor> = Vec::with_capacity(remaining.len());

    while !remaining.is_empty() {
        let ready_idx = remaining.iter().position(|candidate| {
            let after_ok = candidate
                .after
                .iter()
                .all(|dep_name| is_settled(dep_name, &sorted, &remaining));
            let needs_ok = candidate.needs.iter().all(|key| match offered_by.get(key) {
                Some(offerer) => is_settled(offerer.name, &sorted, &remaining),
                None => true,
            });
            after_ok && needs_ok
        });

        match ready_idx {
            Some(idx) => sorted.push(remaining.remove(idx)),
            None => {
                let stuck: Vec<&str> = remaining.iter().map(|d| d.name).collect();
                panic!(
                    "ciclo real de dependência na fase (A) — nenhum destes componentes consegue \
                    progresso: {stuck:?}. Verifique `needs`/`offers`/`after` de cada um."
                );
            }
        }
    }

    sorted
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    /* Struct de teste real, declarada com a macro de verdade — não um mock da forma que
    inventory::submit! deveria ter, e sim o que #[actuator(...)] realmente gera. Prova o caminho
    inteiro: macro → inventory::submit! escondido → attach_discovered_components() descobre →
    StateRegistry cataloga → Composite avalia.
    */
    #[monjolo_macros::actuator(key = "test.discovered.position")]
    struct DiscoveredActuator {
        #[command]
        command: f64,
        #[state]
        position: f64,
    }

    impl DiscoveredActuator {
        fn dynamics(&self) -> f64 {
            self.command() - self.position()
        }
    }

    #[test]
    fn attach_discovered_components_finds_and_wires_a_real_macro_declared_actuator() {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        let config = Snapshot::from_pairs(&[]);

        attach_discovered_components(&mut root, &mut registry.borrow_mut(), &config);
        registry.borrow_mut().resolve().expect("todo input deveria ter provedor");

        assert!(
            root.state_keys().contains(&"test.discovered.position".to_string()),
            "state_keys() de root deveria incluir a chave do componente auto-descoberto: {:?}",
            root.state_keys(),
        );

        let found = registry
            .borrow()
            .actuator("test.discovered.position")
            .expect("deveria ter sido auto-registrado pelo inventory::submit! escondido, sem nenhuma chamada manual a offer_actuator()");

        /* Prova de identidade, não só de ausência de panic: `found` (catálogo de StateRegistry) e
        o que `root` avalia a cada tick precisam ser a MESMA alocação, não duas instâncias
        paralelas de DiscoveredActuator — `construct()` só clona o Rc uma vez, pra offer_actuator();
        o Rc que sobra (não clonado) é o mesmo que entra em `root` via Box::new(Rc<Self>). Nesse
        ponto existem exatamente 3 strong refs: uma dentro do catálogo (o clone de
        `offer_actuator`), uma dentro de `root` (o Rc devolvido por `new()`, movido pro Box sem
        clonar de novo) e esta, `found` (o `.cloned()` que `StateRegistry::actuator()` devolve). Se
        `root` guardasse uma instância SEPARADA, essa contagem seria 2, não 3.
        */
        assert_eq!(
            Rc::strong_count(&found),
            3,
            "catálogo, árvore de avaliação (root) e este handle deveriam compartilhar a mesma \
            alocação — se root tivesse construído uma instância separada, a contagem seria 2",
        );

        found.write(10.0);
        root.evaluate(); // avalia a árvore inteira, inclusive o componente descoberto — sem panic
    }

    /* Sensor de teste real, na mesma chave que DiscoveredActuator já oferece como estado próprio
    (`test.discovered.position`) — um Sensor nunca inventa seu próprio valor bruto, só lê e
    transforma um que já existe (`Sensor::new()` chama `subscribe_read()`, contraparte só-leitura de
    um `#[offer]`/`#[state]` já existente; ver sensor/model.rs). Prova o caminho inteiro pro lado de
    leitura: macro → inventory::submit! escondido → attach_discovered_components() descobre →
    StateRegistry cataloga (Sensor nunca entra em `root`, só cataloga — ver ComponentKind).
    */
    #[monjolo_macros::sensor(key = "test.discovered.position")]
    struct DiscoveredSensor;

    /* Depende do atuador/sensor descobertos pelos testes acima (mesma chave, "test.discovered.
    position" — o sensor lê de volta a própria posição do atuador) — prova que Controller entra na
    fase (C), depois do Actuator (fase B), sem precisar de nenhuma ordem específica entre eles na
    hora de declarar (need_sensor/need_actuator são ordem-independentes, mesma prova de
    controller::model::model::tests::resolves_named_dependencies_declared_before_they_are_offered) —
    e que `control()` (não mais um `evaluate()` vazio) é de fato chamado e consegue ler o sensor e
    escrever no atuador através dos getters gerados pela macro.
    */
    #[monjolo_macros::controller(name = "test_controller")]
    struct TestController {
        #[sensor(key = "test.discovered.position")]
        reading: f64,
        #[actuator(key = "test.discovered.position")]
        position: f64,
    }

    impl TestController {
        fn control(&self) {
            self.position().write(self.reading().read() + 1.0);
        }
    }

    #[test]
    fn attach_discovered_components_finds_and_wires_a_real_macro_declared_controller() {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        let config = Snapshot::from_pairs(&[]);

        attach_discovered_components(&mut root, &mut registry.borrow_mut(), &config);
        registry.borrow_mut().resolve().expect("todo input deveria ter provedor");

        let found = registry
            .borrow()
            .controller("test_controller")
            .expect("deveria ter sido auto-registrado pelo inventory::submit! escondido, sem nenhuma chamada manual a offer_controller()");

        /* Mesma prova de identidade do teste do Actuator acima — catálogo + árvore de avaliação
        (fase C) + este handle compartilham a mesma alocação.
        */
        assert_eq!(
            Rc::strong_count(&found),
            3,
            "catálogo, árvore de avaliação (root, fase C) e este handle deveriam compartilhar a \
            mesma alocação",
        );

        root.evaluate(); // avalia a árvore inteira, inclusive control() (lê sensor, escreve atuador) — sem panic
    }

    /* Prova o mecanismo mais geral: campo escalar (#[state]+#[config]+#[offer]), campo array
    (idem, 3 elementos), campo #[offer]-só com setter (escrito de dentro de evaluate()) e campo
    comum (Default::default(), sem Proxy nenhum) — mesma combinação real usada depois em
    Reactor/Separator/Stripper/Compressor, só menor.
    */
    #[derive(Default)]
    struct Marker;

    #[monjolo_macros::dynamic_model]
    struct DiscoveredDynamicModel {
        #[state]
        #[config(key = "test.dynamic.scalar")]
        #[offer(key = "test.discovered.scalar")]
        scalar: f64,

        #[state]
        #[config(prefix = "test.dynamic.array", components = ["a", "b", "c"])]
        #[offer(prefix = "test.discovered.array", components = ["a", "b", "c"])]
        array: [f64; 3],

        #[offer(key = "test.discovered.output")]
        output: f64,

        _marker: Marker,
    }

    impl DiscoveredDynamicModel {
        fn compute(&self) {
            let sum: f64 = self.array().iter().sum();
            self.set_output(self.scalar() + sum);
        }
    }

    /* Quem calcula a derivada de `DiscoveredDynamicModel::scalar`/`array` nem sempre é o próprio
    dono do valor (Reactor declara `vapor`, mas só Flows tem os quatro subsistemas ao mesmo tempo
    pra saber entrada/saída) — por isso a oferta de ".derivative" é responsabilidade de QUEM
    CALCULA, um `#[offer(...)]` comum onde quer que more esse `compute()`, nunca um campo-irmão
    automático no dono do valor (ver nota em `dynamic_model.rs` da macro). Este struct faz o papel
    de Flows: não é dono de `scalar`/`array`, só oferece as derivadas deles.
    */
    #[monjolo_macros::dynamic_model]
    struct DerivativeOwner {
        #[offer(key = "test.discovered.scalar.derivative")]
        scalar_derivative: f64,
        #[offer(prefix = "test.discovered.array", components = ["a.derivative", "b.derivative", "c.derivative"])]
        array_derivative: [f64; 3],
    }

    impl DerivativeOwner {
        fn compute(&self) {}
    }

    #[test]
    fn attach_discovered_components_finds_and_wires_a_real_macro_declared_dynamic_model() {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        let config = Snapshot::from_pairs(&[
            ("test.dynamic.scalar", 10.0),
            ("test.dynamic.array.a", 1.0),
            ("test.dynamic.array.b", 2.0),
            ("test.dynamic.array.c", 3.0),
        ]);

        attach_discovered_components(&mut root, &mut registry.borrow_mut(), &config);
        registry.borrow_mut().resolve().expect("todo input deveria ter provedor");

        /* Leitura independente, por um segundo subscribe()/resolve() sobre as MESMAS chaves já
        ofertadas — prova que o valor está de fato no StateRegistry (não só num campo privado que
        só o próprio componente enxerga), sem precisar de um getter público em
        DiscoveredDynamicModel além dos que a macro já gera.
        */
        let (_, needed) = registry.borrow_mut().subscribe(
            &[],
            &[
                "test.discovered.scalar",
                "test.discovered.array.a",
                "test.discovered.array.c",
                "test.discovered.output",
            ],
        );
        registry.borrow_mut().resolve().expect("chaves já ofertadas deveriam resolver de novo sem erro");

        root.evaluate(); // avalia a árvore inteira, inclusive o componente descoberto

        assert_eq!(needed[0].get(), 10.0, "scalar deveria ter sido semeado pelo config");
        assert_eq!(needed[1].get(), 1.0, "array[0] (\"a\") deveria ter sido semeado pelo config");
        assert_eq!(needed[2].get(), 3.0, "array[2] (\"c\") deveria ter sido semeado pelo config");
        assert_eq!(
            needed[3].get(),
            16.0,
            "output deveria ser scalar + soma(array) = 10 + (1+2+3), escrito por evaluate()",
        );
    }

    /* Prova o mecanismo que corrige o bug real (Reactor/Separator/Stripper/Compressor nunca
    declaravam state_keys() — RK4 nunca via a química da planta) SEM reintroduzir o bug seguinte
    (auto-ofertar ".derivative" no dono do valor, forçando ownership errada quando quem calcula é
    outro componente): (a) campo "own_state" (#[config]+#[offer]) entra em state_keys() sozinho,
    sem exigir impl manual; (b) a chave ".derivative" correspondente NÃO é reivindicada por
    DiscoveredDynamicModel — um componente diferente (DerivativeOwner, no papel de Flows) consegue
    ofertá-la sem colisão nenhuma, e escrever nela de verdade, no MESMO slot que state_keys()
    aponta.
    */
    #[test]
    fn config_plus_offer_field_declares_state_keys_and_its_derivative_can_be_owned_elsewhere() {
        let registry = StateRegistry::shared();
        let config = Snapshot::from_pairs(&[("test.dynamic.scalar", 5.0)]);
        let model = DiscoveredDynamicModel::new(&mut registry.borrow_mut(), &config);

        assert_eq!(
            model.state_keys(),
            vec![
                "test.discovered.scalar",
                "test.discovered.array.a",
                "test.discovered.array.b",
                "test.discovered.array.c",
            ],
            "state_keys() deveria vir sozinho dos campos #[config]+#[offer], sem impl manual",
        );

        // Componente separado — não DiscoveredDynamicModel — oferece e escreve as derivadas.
        let derivative_owner = DerivativeOwner::new(&mut registry.borrow_mut(), &config);
        derivative_owner.set_scalar_derivative(7.0);
        derivative_owner.set_array_derivative([1.0, 2.0, 3.0]);

        let (_, needed) = registry.borrow_mut().subscribe(
            &[],
            &[
                "test.discovered.scalar.derivative",
                "test.discovered.array.a.derivative",
                "test.discovered.array.c.derivative",
            ],
        );
        registry.borrow_mut().resolve().expect(
            "DerivativeOwner ofertou essas chaves sem colisão nenhuma com DiscoveredDynamicModel",
        );

        assert_eq!(needed[0].get(), 7.0, "DerivativeOwner escreveu no slot real de scalar.derivative");
        assert_eq!(needed[1].get(), 1.0, "DerivativeOwner escreveu no slot real de array.a.derivative");
        assert_eq!(needed[2].get(), 3.0, "DerivativeOwner escreveu no slot real de array.c.derivative");
    }

    /* Prova que #[dynamic_model(after = [...])] ordena de verdade, não só "não dá panic": B lê
    (via #[need]) uma chave que só A escreve. Proxy nasce em 0.0 — se B rodasse antes de A no
    mesmo evaluate(), b_output ficaria 0.0 + 1.0 = 1.0, não 43.0. Nada em `inventory::iter` garante
    que UpstreamA venha antes de DownstreamB por acaso (ordem de registro não segue dependência
    nenhuma) — só `after` explica um resultado correto de forma confiável.
    */
    #[monjolo_macros::dynamic_model]
    struct UpstreamA {
        #[offer(key = "test.order.a_output")]
        output: f64,
    }

    impl UpstreamA {
        fn compute(&self) {
            self.set_output(42.0);
        }
    }

    #[monjolo_macros::dynamic_model(after = ["UpstreamA"])]
    struct DownstreamB {
        #[need(key = "test.order.a_output")]
        a_output: f64,
        #[offer(key = "test.order.b_output")]
        output: f64,
    }

    impl DownstreamB {
        fn compute(&self) {
            self.set_output(self.a_output() + 1.0);
        }
    }

    #[test]
    fn attach_discovered_components_orders_phase_a_by_after() {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        let config = Snapshot::from_pairs(&[]);

        attach_discovered_components(&mut root, &mut registry.borrow_mut(), &config);
        registry.borrow_mut().resolve().expect("todo input deveria ter provedor");

        let (_, needed) = registry.borrow_mut().subscribe(&[], &["test.order.b_output"]);
        registry.borrow_mut().resolve().expect("chave já ofertada deveria resolver de novo sem erro");

        root.evaluate();

        assert_eq!(
            needed[0].get(),
            43.0,
            "DownstreamB deveria ler a_output=42.0 já escrito por UpstreamA no MESMO evaluate(), \
            graças a after = [\"UpstreamA\"] — se desse 1.0, a ordem não foi respeitada",
        );
    }

    /* Prova a máquina inteira de `#[monjolo::tasks]` de ponta a ponta: `TaskDrivenUnit` só existe
    via `#[dynamic_model(tasks)]` (sem `compute()`, sem `after` nenhum declarado) — sua única
    física mora no método `doubled`, marcado `#[monjolo::tasks]`, que lê o PRÓPRIO campo ofertado
    (`level`, semeado por `#[config]`) e publica "test.task_unit.doubled". `DownstreamTaskUnit`,
    uma unidade DIFERENTE, tem uma tarefa (`tripled`) que PRECISA dessa mesma chave — nenhuma
    delas declara `after` uma da outra; a ordem inteira (unidade→sua própria tarefa→tarefa de
    outra unidade) sai só de casar `needs`/`offers`, mais o `after` auto-injetado que garante
    "unidade antes de suas próprias tarefas" (sem ele, `registry.instance::<TaskDrivenUnit>(...)`
    dentro do `construct` de `doubled` poderia rodar antes de `TaskDrivenUnit::new()` chamar
    `offer_instance`). SEM `#[state]` de propósito: `#[dynamic_model]`/`inventory` são globais ao
    binário de teste inteiro — um `#[state]` aqui exigiria uma `".derivative"` ofertada por
    alguém, e vazaria pra QUALQUER outro teste que rode `Simulation::run()`/`attach_discovered_
    components()` no mesmo processo (`#[state]` já tem cobertura própria em outro teste).
    */
    #[monjolo_macros::dynamic_model(tasks)]
    struct TaskDrivenUnit {
        #[config(key = "test.task_unit.state.level")]
        #[offer(key = "test.task_unit.level")]
        level: f64,
    }

    #[monjolo_macros::tasks]
    impl TaskDrivenUnit {
        #[offer(key = "test.task_unit.doubled")]
        fn doubled(&self) -> f64 {
            self.level() * 2.0
        }
    }

    #[monjolo_macros::dynamic_model(tasks)]
    struct DownstreamTaskUnit {}

    #[monjolo_macros::tasks]
    impl DownstreamTaskUnit {
        #[need(key = "test.task_unit.doubled")]
        #[offer(key = "test.task_unit.tripled")]
        fn tripled(&self, doubled: f64) -> f64 {
            doubled * 1.5
        }
    }

    #[test]
    fn tasks_macro_wires_two_units_across_a_needs_offers_boundary_with_no_after_declared() {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        let config = Snapshot::from_pairs(&[("test.task_unit.state.level", 10.0)]);

        attach_discovered_components(&mut root, &mut registry.borrow_mut(), &config);
        registry.borrow_mut().resolve().expect("todo input deveria ter provedor");

        let (_, needed) = registry
            .borrow_mut()
            .subscribe(&[], &["test.task_unit.doubled", "test.task_unit.tripled"]);
        registry.borrow_mut().resolve().expect("chaves já ofertadas deveriam resolver de novo sem erro");

        root.evaluate();

        assert_eq!(
            needed[0].get(),
            20.0,
            "TaskDrivenUnit::doubled deveria ler o próprio #[state] level=10.0 (seedado por \
            #[config]) e publicar 20.0",
        );
        assert_eq!(
            needed[1].get(),
            30.0,
            "DownstreamTaskUnit::tripled deveria ler 20.0 já publicado por TaskDrivenUnit::doubled \
            (SEM after declarado entre as duas unidades) e publicar 30.0",
        );
    }

    /* Prova #[need(prefix = ..., components = [...])] — forma array, usada por Flows (precisa de
    composições de 8 componentes de vários subsistemas ao mesmo tempo). Mesma mecânica de
    #[offer(...)] array, só do lado "needs" de subscribe().
    */
    #[monjolo_macros::dynamic_model]
    struct UpstreamArray {
        #[offer(prefix = "test.array_order.upstream", components = ["a", "b", "c"])]
        values: [f64; 3],
    }

    impl UpstreamArray {
        fn compute(&self) {
            self.set_values([10.0, 20.0, 30.0]);
        }
    }

    #[monjolo_macros::dynamic_model(after = ["UpstreamArray"])]
    struct DownstreamArrayReader {
        #[need(prefix = "test.array_order.upstream", components = ["a", "b", "c"])]
        upstream: [f64; 3],
        #[offer(key = "test.array_order.sum")]
        sum: f64,
    }

    impl DownstreamArrayReader {
        fn compute(&self) {
            let values = self.upstream();
            self.set_sum(values.iter().sum());
        }
    }

    #[test]
    fn need_supports_array_form() {
        let registry = StateRegistry::shared();
        let mut root = Composite::new();
        let config = Snapshot::from_pairs(&[]);

        attach_discovered_components(&mut root, &mut registry.borrow_mut(), &config);
        registry.borrow_mut().resolve().expect("todo input deveria ter provedor");

        let (_, needed) = registry.borrow_mut().subscribe(&[], &["test.array_order.sum"]);
        registry.borrow_mut().resolve().expect("chave já ofertada deveria resolver de novo sem erro");

        root.evaluate();

        assert_eq!(
            needed[0].get(),
            60.0,
            "DownstreamArrayReader deveria ler [10,20,30] de UpstreamArray via #[need(prefix=..., \
            components=[...])] e somar, dando 60.0",
        );
    }

    /* Testa `sort_phase_a` diretamente (função privada, sem passar por inventory/macro) — pergunta
    concreta: numa forma "diamante" (Y1 e Y2 são IRMÃOS, ambos after=["X"], sem relação entre si; Z
    só lista after=["Y1"], NUNCA "Y2"), Z acaba mesmo depois de Y2, só porque Y2 é irmão de algo que
    Z precisa? Ou isso não é garantido? Continua valendo sem `needs`/`offers` populados (`&[]` pros
    quatro) — o desempate por `after` sozinho se comporta EXATAMENTE como o sort antigo quando não
    há grafo de dado nenhum por trás.
    */
    fn noop_construct(_: &mut StateRegistry, _: &Snapshot) -> Option<Box<dyn DynamicModel>> {
        None
    }

    #[test]
    fn sort_phase_a_does_not_guarantee_order_against_an_unlisted_sibling() {
        static X: ComponentDescriptor = ComponentDescriptor {
            name: "X",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };
        static Y1: ComponentDescriptor = ComponentDescriptor {
            name: "Y1",
            kind: ComponentKind::Dynamic,
            after: &["X"],
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };
        static Y2: ComponentDescriptor = ComponentDescriptor {
            name: "Y2",
            kind: ComponentKind::Dynamic,
            after: &["X"],
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };
        static Z: ComponentDescriptor = ComponentDescriptor {
            name: "Z",
            kind: ComponentKind::Dynamic,
            after: &["Y1"], // Z NUNCA lista Y2, só Y1
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };

        // Ordem adversária: Z é o PRIMEIRO candidato considerado a cada rodada, Y2 o ÚLTIMO —
        // exatamente o cenário que faz Z "furar a fila" na frente do irmão que ele não listou.
        let sorted = sort_phase_a(vec![&Z, &X, &Y1, &Y2]);
        let position = |name: &str| sorted.iter().position(|d| d.name == name).unwrap();

        assert!(position("X") < position("Y1"), "X sempre antes de Y1 (listado)");
        assert!(position("Y1") < position("Z"), "Y1 sempre antes de Z (listado)");
        assert!(
            position("Z") < position("Y2"),
            "Z termina ANTES do irmão não-listado Y2 nesta ordem adversária — prova que `after` \
            só garante ordem contra nomes EXPLICITAMENTE listados, nunca contra irmãos \
            transitivos não mencionados. Se esta asserção falhar (Y2 antes de Z), o algoritmo \
            mudou de comportamento e os comentários em flows.rs/heat.rs/measurements.rs sobre \
            listar todas as dependências diretas precisam ser revisados.",
        );
    }

    /* Contraste com o teste acima: cadeia PURA (sem irmãos disputando o mesmo pai) — aí sim
    transitividade vem de graça, mesmo listando só o vizinho imediato.
    */
    #[test]
    fn sort_phase_a_handles_pure_chains_transitively_for_free() {
        static A: ComponentDescriptor = ComponentDescriptor {
            name: "ChainA",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };
        static B: ComponentDescriptor = ComponentDescriptor {
            name: "ChainB",
            kind: ComponentKind::Dynamic,
            after: &["ChainA"],
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };
        static C: ComponentDescriptor = ComponentDescriptor {
            name: "ChainC",
            kind: ComponentKind::Dynamic,
            after: &["ChainB"], // não lista ChainA — só o vizinho imediato
            needs: &[],
            offers: &[],
            construct: noop_construct,
        };

        // Ordem adversária: C primeiro.
        let sorted = sort_phase_a(vec![&C, &A, &B]);
        let position = |name: &str| sorted.iter().position(|d| d.name == name).unwrap();

        assert!(position("ChainA") < position("ChainB"));
        assert!(
            position("ChainB") < position("ChainC"),
            "cadeia pura: A→B→C — C só lista B, mas como não há irmão nenhum disputando o lugar \
            de B, A acaba garantido antes de C também, de graça",
        );
        assert!(position("ChainA") < position("ChainC"));
    }

    /* Prova o caso concreto que motivou trocar `after` manual por um grafo automático de
    `needs`/`offers`: duas tarefas de UNIDADES DIFERENTES, cada uma precisando de uma chave que a
    OUTRA oferece (Reactor::outlet_flow precisa de "separator.pressure"; Separator::pressure
    precisa de "reactor.temperature") — não é um ciclo de verdade porque as CHAVES não se repetem
    (reactor.temperature != separator.pressure), só pareceria um ciclo se a ordenação fosse feita
    no grão de "unidade inteira" em vez de "tarefa". Nenhum `after` é declarado — a ordem sai
    inteira do casamento de chave.
    */
    #[test]
    fn sort_phase_a_orders_cross_unit_tasks_by_matching_needs_to_offers_with_no_after_declared() {
        static REACTOR_TEMPERATURE: ComponentDescriptor = ComponentDescriptor {
            name: "Reactor::temperature",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &[],
            offers: &["reactor.temperature"],
            construct: noop_construct,
        };
        static SEPARATOR_PRESSURE: ComponentDescriptor = ComponentDescriptor {
            name: "Separator::pressure",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &["reactor.temperature"],
            offers: &["separator.pressure"],
            construct: noop_construct,
        };
        static REACTOR_OUTLET_FLOW: ComponentDescriptor = ComponentDescriptor {
            name: "Reactor::outlet_flow",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &["separator.pressure"],
            offers: &["flows.stream7"],
            construct: noop_construct,
        };

        // Ordem adversária: outlet_flow primeiro, temperature por último.
        let sorted = sort_phase_a(vec![&REACTOR_OUTLET_FLOW, &SEPARATOR_PRESSURE, &REACTOR_TEMPERATURE]);
        let position = |name: &str| sorted.iter().position(|d| d.name == name).unwrap();

        assert!(
            position("Reactor::temperature") < position("Separator::pressure"),
            "Separator::pressure precisa de reactor.temperature",
        );
        assert!(
            position("Separator::pressure") < position("Reactor::outlet_flow"),
            "Reactor::outlet_flow precisa de separator.pressure",
        );
    }

    /* Contraste com o teste acima: um ciclo REAL (A precisa do que B oferece, B precisa do que A
    oferece) deve travar o bootstrap com uma mensagem clara, nunca ser silenciosamente despejado
    numa ordem arbitrária (comportamento antigo) nem deixado pra `StateRegistry::resolve()`
    detectar — `resolve()` não consegue: as duas chaves TÊM ofertante, só não em ordem executável.
    */
    #[test]
    #[should_panic(expected = "ciclo real de dependência")]
    fn sort_phase_a_panics_on_a_real_cycle() {
        static A: ComponentDescriptor = ComponentDescriptor {
            name: "A",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &["b.output"],
            offers: &["a.output"],
            construct: noop_construct,
        };
        static B: ComponentDescriptor = ComponentDescriptor {
            name: "B",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &["a.output"],
            offers: &["b.output"],
            construct: noop_construct,
        };

        sort_phase_a(vec![&A, &B]);
    }

    /* Duas chaves iguais ofertadas por descritores diferentes é bug de programação (não dado de
    usuário em runtime) — precisa travar o bootstrap, não sobrescrever um índice silenciosamente.
    */
    #[test]
    #[should_panic(expected = "ofertada por dois componentes")]
    fn sort_phase_a_panics_on_duplicate_offer() {
        static A: ComponentDescriptor = ComponentDescriptor {
            name: "A",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &[],
            offers: &["duplicated.key"],
            construct: noop_construct,
        };
        static B: ComponentDescriptor = ComponentDescriptor {
            name: "B",
            kind: ComponentKind::Dynamic,
            after: &[],
            needs: &[],
            offers: &["duplicated.key"],
            construct: noop_construct,
        };

        sort_phase_a(vec![&A, &B]);
    }
}
