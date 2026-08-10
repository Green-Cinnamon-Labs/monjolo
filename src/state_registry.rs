/** monjolo/state_registry.rs

StateRegistry (ver docs/issue55_opcua_refactor/plan_refactor.md, seções 1.3, 6 e 7) — raiz única de
registro e resolução da simulação. Guarda dois mundos, sempre distintos, pro estado numérico bruto:
CurrentState (`current_state`) — o estado real, confirmado, persistido. Compartilhável entre threads
(Art. 1.3 §1º do plano legislativo): é o "último estado físico confirmado" — a Thread do Adaptador
(e, no futuro, um Controlador) lê direto daqui, nunca de EvaluationState. EvaluationState
(`evaluation_state`) — a cópia de trabalho onde todo Proxy lê/escreve durante uma rodada de
avaliação. Pode conter valores "hipotéticos" (chute intermediário de um solver iterativo) até
alguém decidir que aquela rodada está ok. Thread-local — só a Thread da planta toca, sem lock, sem
sincronização: `Rc<RefCell<Vec<Cell<f64>>>>`.

Além dos slots numéricos, este mesmo tipo também é o catálogo de sensores, atuadores e controllers
nomeados da simulação — não um registry separado por categoria. `Sensor`/`Actuator`/`Controller` têm
semântica diferente de um slot `f64` (catálogo write-once no offer, nunca mutado depois, sem
`set()`), então ganham seus próprios campos/métodos (`offer_sensor`/`need_sensor`,
`offer_actuator`/`need_actuator`, `offer_controller`), mas o ciclo é o mesmo declare → register →
resolve → inject de sempre, fechado pela mesma chamada a `resolve()`. Ver `SensorHandle`/
`ActuatorHandle` — mesmo truque de `Proxy`/`ReadProxy` (Art. 7.1): nascem sem resolução, `resolve()`
escreve o índice real neles.

`commit()` é o commit EvaluationState -> CurrentState — mecânico, só copia, mais o avanço de
`generation` (Art. 3.6.2). A decisão de QUANDO chamar (ex.: depois que um passo do Integrator
convergiu) não é do StateRegistry, é de quem orquestra a simulação.
*/
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use crate::actuator::Actuator;
use crate::controller::Controller;
use crate::sensor::Sensor;

/** Uma entrada nomeada: nome semântico + valor, numa posição (implícita pelo lugar no `Vec` que a
contém — não redeclarada aqui).

`StateSlot` NÃO é o buffer quente de leitura/escrita — esse papel é do
`current_state`/`evaluation_state` internos, `Vec<Cell<f64>>` puro, sem nome nenhum embutido.
`StateSlot` só existe pra reconstrução sob demanda (ver `StateRegistry::snapshot()`):
metadado/catálogo pra inspeção, debug, listagem de sinais ou exportação nomeada — nunca o caminho
por onde `Proxy`/`ReadProxy` leem ou escrevem. Resolver `key -> posição` de verdade, em tempo real,
é sempre trabalho do `index: HashMap<String, usize>`, nunca de vasculhar `Vec<StateSlot>`.

Invariante: as posições são append-only. Uma vez que um slot é registrado, sua posição nunca muda
nem é reaproveitada — o que permite resolver uma `key` para uma posição UMA ÚNICA VEZ e confiar
nessa posição para sempre.
*/
pub struct StateSlot {
    pub key: String,
    pub value: f64,
}

/** Handle autossuficiente pra uma posição no buffer de avaliação — carrega o buffer compartilhado
(`Rc<RefCell<Vec<Cell<f64>>>>`) e o índice (`Rc<Cell<usize>>`) juntos, então `get()`/`set()` não
precisam de nada externo passado por parâmetro. Nasce sem resolução (`index = usize::MAX`);
`StateRegistry::resolve()` escreve o índice real nele. Todo clone de um `Proxy` aponta pro mesmo
`Cell`, então resolver uma vez basta — o componente guarda seu clone desde a inscrição e nunca mais
precisa perguntar pelo nome de novo, nem receber o buffer de fora em cada `evaluate()`.

Agnóstico a se o valor por trás é "hipotético" (chute intermediário de um solver iterativo) ou
"real" (convergido) — só endereça a posição.
*/
#[derive(Clone)]
pub struct Proxy {
    buffer: Rc<RefCell<Vec<Cell<f64>>>>,
    index: Rc<Cell<usize>>,
}

impl Proxy {
    fn resolved(buffer: Rc<RefCell<Vec<Cell<f64>>>>, index: usize) -> Self {
        Self {
            buffer,
            index: Rc::new(Cell::new(index)),
        }
    }

    fn unresolved(buffer: Rc<RefCell<Vec<Cell<f64>>>>) -> Self {
        Self {
            buffer,
            index: Rc::new(Cell::new(usize::MAX)),
        }
    }

    fn index(&self) -> usize {
        let idx = self.index.get();
        debug_assert!(
            idx != usize::MAX,
            "Proxy usado antes de StateRegistry::resolve()"
        );
        idx
    }

    pub fn get(&self) -> f64 {
        self.buffer.borrow()[self.index()].get()
    }

    pub fn set(&self, value: f64) {
        self.buffer.borrow()[self.index()].set(value);
    }
}

/** Estado confirmado de verdade, por trás de `Arc<RwLock<...>>` — o "último estado físico
confirmado da planta" (Art. 1.3 §1º do plano legislativo). `generation` avança exatamente uma vez
por `commit()` (nunca por escrita individual): é o que permite a um leitor de fora saber se dois
valores lidos vieram do mesmo tick confirmado ou de ticks diferentes, sem comparar os valores em si
— usado por `Sensor` (Art. 3.6.6) pra cache de idempotência de `SensorBehavior`.
*/
struct CurrentState {
    generation: u64,
    values: Vec<f64>,
}

/** Handle resolvido em duas fases sobre `CurrentState` — a contraparte de `Proxy` só-leitura,
mesmo mecanismo (Art. 7.1): nasce sem resolução (`index = usize::MAX`), `StateRegistry::resolve()`
escreve o índice real nele depois. Estruturalmente parecido a `Proxy` (buffer + índice), mas um
tipo à parte de propósito: `Proxy` pode endereçar `EvaluationState`, que pode conter valor
hipotético de um solver iterativo em andamento (seção 7.2 do plano); `ReadProxy` só existe sobre
`CurrentState`, sempre o último valor confirmado. Misturar os dois tipos não compila — é assim que
essa garantia vira uma propriedade do tipo, não uma regra de disciplina de quem usa.

`index: Arc<AtomicUsize>`, não `Rc<Cell<usize>>` como em `Proxy`: `ReadProxy` precisa atravessar
thread (é o que `Sensor` usa, compartilhável via `Arc<Sensor>`), e `Rc`/`Cell` não são `Sync`. Só
existe `unresolved()` — `ReadProxy` nunca tem lado "offer": ninguém oferece um `ReadProxy`, só
`Sensor` pede um (`StateRegistry::subscribe_read()`).
*/
#[derive(Clone)]
pub struct ReadProxy {
    buffer: Arc<RwLock<CurrentState>>,
    index: Arc<AtomicUsize>,
}

impl ReadProxy {
    fn unresolved(buffer: Arc<RwLock<CurrentState>>) -> Self {
        Self {
            buffer,
            index: Arc::new(AtomicUsize::new(usize::MAX)),
        }
    }

    fn index(&self) -> usize {
        let idx = self.index.load(Ordering::SeqCst);
        debug_assert!(
            idx != usize::MAX,
            "ReadProxy usado antes de StateRegistry::resolve()"
        );
        idx
    }

    /* Leitura simples, sem versão — quem só quer o valor bruto confirmado. */
    pub fn get(&self) -> f64 {
        self.buffer
            .read()
            .expect("CurrentState: lock envenenado")
            .values[self.index()]
    }

    /** Leitura com `generation` — `(geração do CurrentState no momento da leitura, valor)`,
    resolvidos sob o mesmo lock (nunca podem divergir entre si). Usado por `Sensor::read()` (Art.
    3.6.6) pra decidir se `SensorBehavior` precisa rodar de novo ou se o cache do tick ainda vale.
    */
    pub fn get_versioned(&self) -> (u64, f64) {
        let idx = self.index();
        let guard = self.buffer.read().expect("CurrentState: lock envenenado");
        (guard.generation, guard.values[idx])
    }
}

/** Handle resolvido em duas fases sobre o catálogo de sensores nomeados — mesmo mecanismo de
`Proxy`/`ReadProxy` (Art. 7.1), só que sobre `Arc<dyn Sensor>` em vez de `f64`: nasce sem resolução,
`StateRegistry::resolve()` escreve o índice real nele. `Sensor` é `Send + Sync` de verdade (ver
`sensor/model.rs`), então o catálogo por trás é `Arc<dyn Sensor>` — pode atravessar thread.
*/
#[derive(Clone)]
pub struct SensorHandle {
    catalog: Rc<RefCell<Vec<Arc<dyn Sensor>>>>,
    index: Rc<Cell<usize>>,
}

impl SensorHandle {
    fn unresolved(catalog: Rc<RefCell<Vec<Arc<dyn Sensor>>>>) -> Self {
        Self {
            catalog,
            index: Rc::new(Cell::new(usize::MAX)),
        }
    }

    fn index(&self) -> usize {
        let idx = self.index.get();
        debug_assert!(
            idx != usize::MAX,
            "SensorHandle usado antes de StateRegistry::resolve()"
        );
        idx
    }

    pub fn sensor(&self) -> Arc<dyn Sensor> {
        self.catalog.borrow()[self.index()].clone()
    }
}

/** Mesma ideia de `SensorHandle`, pro catálogo de atuadores nomeados — `Rc<dyn Actuator>`, não
`Arc`: ao contrário de `Sensor`, `Actuator` não tem garantia nenhuma de `Send + Sync` — os
atuadores concretos do tep-plant guardam `Proxy` (`Rc`-based), portanto são `!Send`/`!Sync` de
verdade. Usar `Arc` aqui prometeria uma travessia de thread que o tipo concreto não sustenta.
*/
#[derive(Clone)]
pub struct ActuatorHandle {
    catalog: Rc<RefCell<Vec<Rc<dyn Actuator>>>>,
    index: Rc<Cell<usize>>,
}

impl ActuatorHandle {
    fn unresolved(catalog: Rc<RefCell<Vec<Rc<dyn Actuator>>>>) -> Self {
        Self {
            catalog,
            index: Rc::new(Cell::new(usize::MAX)),
        }
    }

    fn index(&self) -> usize {
        let idx = self.index.get();
        debug_assert!(
            idx != usize::MAX,
            "ActuatorHandle usado antes de StateRegistry::resolve()"
        );
        idx
    }

    pub fn actuator(&self) -> Rc<dyn Actuator> {
        self.catalog.borrow()[self.index()].clone()
    }
}

pub struct StateRegistry {
    /** Buffer do estado confirmado (CurrentState, seção 1.3 do plano) —
    `Arc<RwLock<CurrentState>>`, escrito de uma vez só por `commit()` (nunca célula-a-célula): um
    único `write()` cobre valores E geração na mesma seção crítica, então quem lê nunca vê os dois
    dessincronizados. É sobre este buffer que `ReadProxy` resolve, em duas fases, a posição que vai
    ler pra sempre. Sem nome embutido — nome é só em `index`; ver `snapshot()` pra reconstrução
    nomeada sob demanda.
    */
    current_state: Arc<RwLock<CurrentState>>,

    /** Buffer de trabalho de uma rodada de avaliação (seção 8 do plano). Compartilhado com todo
    `Proxy` já emitido — por isso `Rc<RefCell<_>>` (a lista cresce durante subscribe(), então
    precisa de mutabilidade; `Cell` por elemento é o que permite `evaluate()` escrever com `&self`).
    */
    evaluation_state: Rc<RefCell<Vec<Cell<f64>>>>,

    /* nome semântico -> posição em `evaluation_state`, preenchido conforme os outputs vão sendo
    oferecidos em subscribe(). Também é o mapa que `subscribe_read()`/`ReadProxy` resolvem contra —
    um único namespace de nomes pra estado bruto, não dois.
    */
    index: HashMap<String, usize>,

    /* Inputs declarados em subscribe(), ainda não resolvidos. resolve() esvazia essa lista,
    escrevendo a posição real em cada Proxy.
    */
    pending_requests: Vec<(String, Proxy)>,

    /* Mesmo papel de pending_requests, pro lado ReadProxy/subscribe_read() — needs declarados,
    ainda não resolvidos contra `index`.
    */
    pending_read_requests: Vec<(String, ReadProxy)>,

    /* Catálogo de sensores nomeados: valores + índice de busca por nome — mesma forma do par
    evaluation_state/index acima, só que pra Arc<dyn Sensor> em vez de f64.
    */
    sensor_catalog: Rc<RefCell<Vec<Arc<dyn Sensor>>>>,
    sensor_index: HashMap<String, usize>,
    pending_sensor_requests: Vec<(String, SensorHandle)>,

    /* Mesma ideia acima, pro catálogo de atuadores nomeados. */
    actuator_catalog: Rc<RefCell<Vec<Rc<dyn Actuator>>>>,
    actuator_index: HashMap<String, usize>,
    pending_actuator_requests: Vec<(String, ActuatorHandle)>,

    /* Catálogo de controllers nomeados — só lado "offer". Nada hoje depende de buscar um
    Controller por nome (controller.rs ainda não tem design fechado pra isso), então não há
    pending_requests nem Handle aqui, só registro pra descoberta.
    */
    controller_catalog: Rc<RefCell<Vec<Rc<dyn Controller>>>>,
    controller_index: HashMap<String, usize>,
}

impl StateRegistry {
    fn new() -> Self {
        Self {
            current_state: Arc::new(RwLock::new(CurrentState {
                generation: 0,
                values: Vec::new(),
            })),
            evaluation_state: Rc::new(RefCell::new(Vec::new())),
            index: HashMap::new(),
            pending_requests: Vec::new(),
            pending_read_requests: Vec::new(),
            sensor_catalog: Rc::new(RefCell::new(Vec::new())),
            sensor_index: HashMap::new(),
            pending_sensor_requests: Vec::new(),
            actuator_catalog: Rc::new(RefCell::new(Vec::new())),
            actuator_index: HashMap::new(),
            pending_actuator_requests: Vec::new(),
            controller_catalog: Rc::new(RefCell::new(Vec::new())),
            controller_index: HashMap::new(),
        }
    }

    /** Garante que `current_state` tenha, no mínimo, o tamanho de `evaluation_state` — só cresce,
    nunca encolhe (mesma invariante append-only da seção 5.2 do plano). Chamado em `resolve()` (pra
    `ReadProxy` já nascer endereçando uma posição válida, mesmo antes do primeiro `commit()`) e em
    `commit()` (defensivo, custo ~zero depois da primeira vez). Não avança `generation` — não é
    commit de valor nenhum, só reserva espaço.
    */
    fn ensure_current_capacity(&self) {
        let len = self.evaluation_state.borrow().len();
        let mut cur = self.current_state.write().expect("CurrentState: lock envenenado");
        while cur.values.len() < len {
            cur.values.push(0.0);
        }
    }

    /** Único jeito de obter um StateRegistry — não existe construtor público que devolva um valor
    solto. `shared()` sempre embrulha em `Rc<RefCell<_>>`, então todo `DynamicModel` que se inscreve
    guarda um clone do mesmo `Rc` (barato — só incrementa o contador de referência), apontando pra a
    mesma instância. Isso é o que faz dele um singleton de fato: não é uma única instância *global*,
    é uma única instância *por simulação*, garantida pelo tipo — não por disciplina de quem usa.
    */
    pub fn shared() -> Rc<RefCell<StateRegistry>> {
        Rc::new(RefCell::new(Self::new()))
    }

    /** Um DynamicModel se inscreve: `offers` são os nomes dos slots que ele próprio provê
    (reservados e resolvidos na hora — a posição já é conhecida no momento em que a posição é
    criada); `needs` são as chaves de outros componentes que ele vai ler (devolvidas como Proxy NÃO
    resolvido — só ganham posição real em resolve()). Não importa a ordem de inscrição entre quem
    oferece e quem pede.
    */
    pub fn subscribe(&mut self, offers: &[&str], needs: &[&str]) -> (Vec<Proxy>, Vec<Proxy>) {
        let offered = offers
            .iter()
            .map(|&key| {
                let idx = self.evaluation_state.borrow().len();
                self.evaluation_state.borrow_mut().push(Cell::new(0.0));
                self.index.insert(key.to_string(), idx);
                Proxy::resolved(self.evaluation_state.clone(), idx)
            })
            .collect();

        let requested = needs
            .iter()
            .map(|&key| {
                let proxy = Proxy::unresolved(self.evaluation_state.clone());
                self.pending_requests.push((key.to_string(), proxy.clone()));
                proxy
            })
            .collect();

        (offered, requested)
    }

    /** Mesma ideia de `subscribe()`, pro lado `ReadProxy`/`CurrentState`: `Sensor` só tem `needs`,
    nunca `offers` — ninguém oferece um `ReadProxy`, só pede. Resolvido contra o mesmo `index:
    HashMap<String, usize>` que os `offers` de `subscribe()` já preenchem — um único namespace de
    nomes pra estado bruto, não um segundo mapa.
    */
    pub fn subscribe_read(&mut self, needs: &[&str]) -> Vec<ReadProxy> {
        needs
            .iter()
            .map(|&key| {
                let proxy = ReadProxy::unresolved(self.current_state.clone());
                self.pending_read_requests.push((key.to_string(), proxy.clone()));
                proxy
            })
            .collect()
    }

    /** Registra um `Sensor` já construído sob um nome — imediato, mesmo papel de um `offers` de
    `subscribe()`: a posição no catálogo já é conhecida no momento em que é criada.

    `pub(crate)`, não `pub`: quem monta a planta nunca chama isso diretamente — `Sensor::new()`
    (`sensor/model.rs`) já chama, internamente, sob a própria `key`, e devolve o `Arc` resultante.
    "Criado = já oferecido" é uma invariante do tipo, não uma etapa manual de quem constrói.
    */
    pub(crate) fn offer_sensor(&mut self, name: &str, sensor: Arc<dyn Sensor>) {
        let idx = self.sensor_catalog.borrow().len();
        self.sensor_catalog.borrow_mut().push(sensor);
        self.sensor_index.insert(name.to_string(), idx);
    }

    /** Declara a necessidade de um `Sensor` nomeado — devolve um `SensorHandle` NÃO resolvido; só
    fica válido depois que `resolve()` rodar e algum `offer_sensor()` tiver oferecido esse nome.
    */
    pub fn need_sensor(&mut self, name: &str) -> SensorHandle {
        let handle = SensorHandle::unresolved(self.sensor_catalog.clone());
        self.pending_sensor_requests.push((name.to_string(), handle.clone()));
        handle
    }

    /* Nomes de todos os sensores já registrados — descoberta, pra quem (ex.: um futuro adaptador
    de rede, rodando dentro da mesma Thread) precisa listar o que existe sem conhecer os nomes de
    antemão.
    */
    pub fn sensor_names(&self) -> impl Iterator<Item = &str> {
        self.sensor_index.keys().map(String::as_str)
    }

    /* Busca um sensor já resolvido pelo nome. `None` se o nome não existe. */
    pub fn sensor(&self, name: &str) -> Option<Arc<dyn Sensor>> {
        let idx = *self.sensor_index.get(name)?;
        self.sensor_catalog.borrow().get(idx).cloned()
    }

    /* Mesma ideia de offer_sensor()/need_sensor()/sensor_names()/sensor(), pro catálogo de
    atuadores nomeados — `pub(crate)` pelo mesmo motivo: só `Actuator::new()`
    (`actuator/model.rs`) chama isso, sob a própria `key`.
    */
    pub(crate) fn offer_actuator(&mut self, name: &str, actuator: Rc<dyn Actuator>) {
        let idx = self.actuator_catalog.borrow().len();
        self.actuator_catalog.borrow_mut().push(actuator);
        self.actuator_index.insert(name.to_string(), idx);
    }

    pub fn need_actuator(&mut self, name: &str) -> ActuatorHandle {
        let handle = ActuatorHandle::unresolved(self.actuator_catalog.clone());
        self.pending_actuator_requests.push((name.to_string(), handle.clone()));
        handle
    }

    pub fn actuator_names(&self) -> impl Iterator<Item = &str> {
        self.actuator_index.keys().map(String::as_str)
    }

    pub fn actuator(&self, name: &str) -> Option<Rc<dyn Actuator>> {
        let idx = *self.actuator_index.get(name)?;
        self.actuator_catalog.borrow().get(idx).cloned()
    }

    /** Registra um `Controller` já construído sob um nome — só lado "offer". Nada hoje depende de
    buscar um Controller pelo nome (`controller/mod.rs` ainda não tem design fechado pra isso),
    então não há `need_controller()`: um Controller concreto resolve as próprias dependências de
    `Sensor`/`Actuator` via `need_sensor()`/`need_actuator()` normalmente, na própria construção —
    isso aqui é só o catálogo pra descoberta. `pub(crate)`, mesmo motivo de `offer_sensor`/
    `offer_actuator`: só `Controller::new()` (`controller/model.rs`) chama isso, sob o `name` que
    recebeu.
    */
    pub(crate) fn offer_controller(&mut self, name: &str, controller: Rc<dyn Controller>) {
        let idx = self.controller_catalog.borrow().len();
        self.controller_catalog.borrow_mut().push(controller);
        self.controller_index.insert(name.to_string(), idx);
    }

    pub fn controller_names(&self) -> impl Iterator<Item = &str> {
        self.controller_index.keys().map(String::as_str)
    }

    pub fn controller(&self, name: &str) -> Option<Rc<dyn Controller>> {
        let idx = *self.controller_index.get(name)?;
        self.controller_catalog.borrow().get(idx).cloned()
    }

    /** Roda uma única vez, depois que tudo já se registrou — slots, sensores e atuadores. Resolve
    cada pendência contra a posição já conhecida (de quem ofereceu aquele nome). Se algum input não
    tiver provedor, é erro — o resto pode ter ficado parcialmente resolvido, então não adianta
    continuar rodando a simulação depois disso falhar.
    */
    pub fn resolve(&mut self) -> Result<(), String> {
        for (key, proxy) in &self.pending_requests {
            match self.index.get(key) {
                Some(&idx) => proxy.index.set(idx),
                None => {
                    return Err(format!(
                    "input '{key}' declarado em subscribe() mas nenhum componente oferece esse slot"
                ))
                }
            }
        }
        for (key, proxy) in &self.pending_read_requests {
            match self.index.get(key) {
                Some(&idx) => proxy.index.store(idx, Ordering::SeqCst),
                None => {
                    return Err(format!(
                        "input '{key}' declarado em subscribe_read() mas nenhum componente oferece esse slot"
                    ))
                }
            }
        }
        for (name, handle) in &self.pending_sensor_requests {
            match self.sensor_index.get(name) {
                Some(&idx) => handle.index.set(idx),
                None => {
                    return Err(format!(
                        "sensor '{name}' declarado em need_sensor() mas nenhum offer_sensor() com esse nome"
                    ))
                }
            }
        }
        for (name, handle) in &self.pending_actuator_requests {
            match self.actuator_index.get(name) {
                Some(&idx) => handle.index.set(idx),
                None => {
                    return Err(format!(
                        "atuador '{name}' declarado em need_actuator() mas nenhum offer_actuator() com esse nome"
                    ))
                }
            }
        }
        self.ensure_current_capacity();
        Ok(())
    }

    /** Lê o valor já commitado de uma chave em CurrentState — leitura pontual por string, útil pra
    debug/inspeção avulsa. Nunca durante evaluate(), só depois que um passo já fechou. `Sensor` não
    deve usar isso no caminho quente — ver `subscribe_read()`. None se a chave não existe ou se
    nenhum commit() rodou ainda.
    */
    pub fn read(&self, key: &str) -> Option<f64> {
        let idx = *self.index.get(key)?;
        let cur = self.current_state.read().expect("CurrentState: lock envenenado");
        cur.values.get(idx).copied()
    }

    /** Foto nomeada do CurrentState — reconstrói `Vec<StateSlot>` sob demanda a partir de `index` +
    o buffer atual. Não é o armazenamento principal (esse é `current_state`, um `Vec<f64>` cru por
    trás do `RwLock`); é metadado/catálogo pra inspeção, debug, listagem de sinais ou exportação —
    não o caminho quente de leitura/escrita.
    */
    pub fn snapshot(&self) -> Vec<StateSlot> {
        let cur = self.current_state.read().expect("CurrentState: lock envenenado");
        let mut slots: Vec<StateSlot> = (0..cur.values.len())
            .map(|_| StateSlot {
                key: String::new(),
                value: 0.0,
            })
            .collect();
        for (key, &idx) in &self.index {
            if let Some(slot) = slots.get_mut(idx) {
                slot.key = key.clone();
                slot.value = cur.values[idx];
            }
        }
        slots
    }

    /** Commit EvaluationState -> CurrentState (Art. 3.6.2 do plano legislativo): um único `write()`
    lock cobre a cópia inteira e o avanço de `generation` — nunca célula-a-célula. É esse "uma vez
    por tick, tudo junto" que dá a `current_state` a propriedade de "último estado físico
    confirmado": ninguém de fora consegue observar uma mistura de valores de ticks diferentes entre
    variáveis distintas, nem uma `generation` que já avançou mas com valores que ainda não. Não
    decide nada sobre SE deve commitar — só copia o que está lá no momento em que é chamado.
    */
    pub fn commit(&mut self) {
        let eval = self.evaluation_state.borrow();
        let mut cur = self.current_state.write().expect("CurrentState: lock envenenado");
        if cur.values.len() < eval.len() {
            cur.values.resize(eval.len(), 0.0);
        }
        for i in 0..eval.len() {
            cur.values[i] = eval[i].get();
        }
        cur.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummySensor;
    impl Sensor for DummySensor {
        fn read(&self) -> f64 {
            42.0
        }
    }

    struct DummyActuator;
    impl Actuator for DummyActuator {
        fn write(&self, _value: f64) {}
    }

    struct DummyController;
    impl Controller for DummyController {}

    /* Prova o ponto central desta mudança pro lado sensor: need_sensor() antes de offer_sensor()
    funciona — a ordem entre quem pede e quem oferece não importa, exatamente como já valia pra
    Proxy/subscribe() (Art. 6.3).
    */
    #[test]
    fn need_sensor_resolves_regardless_of_order_relative_to_offer() {
        let mut registry = StateRegistry::new();
        let handle = registry.need_sensor("reactor_pressure");
        registry.offer_sensor("reactor_pressure", Arc::new(DummySensor));
        registry.resolve().unwrap();
        assert_eq!(handle.sensor().read(), 42.0);
    }

    #[test]
    fn two_need_sensor_calls_for_the_same_name_share_the_same_instance() {
        let mut registry = StateRegistry::new();
        let handle_a = registry.need_sensor("reactor_pressure");
        let handle_b = registry.need_sensor("reactor_pressure");
        registry.offer_sensor("reactor_pressure", Arc::new(DummySensor));
        registry.resolve().unwrap();
        assert!(Arc::ptr_eq(&handle_a.sensor(), &handle_b.sensor()));
    }

    #[test]
    fn resolve_errors_when_a_needed_sensor_was_never_offered() {
        let mut registry = StateRegistry::new();
        registry.need_sensor("missing");
        assert!(registry.resolve().is_err());
    }

    #[test]
    fn need_actuator_resolves_regardless_of_order_relative_to_offer() {
        let mut registry = StateRegistry::new();
        let handle = registry.need_actuator("purge");
        registry.offer_actuator("purge", Rc::new(DummyActuator));
        registry.resolve().unwrap();
        handle.actuator().write(10.0);
    }

    #[test]
    fn two_need_actuator_calls_for_the_same_name_share_the_same_instance() {
        let mut registry = StateRegistry::new();
        let handle_a = registry.need_actuator("purge");
        let handle_b = registry.need_actuator("purge");
        registry.offer_actuator("purge", Rc::new(DummyActuator));
        registry.resolve().unwrap();
        assert!(Rc::ptr_eq(&handle_a.actuator(), &handle_b.actuator()));
    }

    #[test]
    fn resolve_errors_when_a_needed_actuator_was_never_offered() {
        let mut registry = StateRegistry::new();
        registry.need_actuator("missing");
        assert!(registry.resolve().is_err());
    }

    /* Controller só tem lado "offer" — nada pede um Controller por nome, então não há resolve()
    pendente pra testar aqui, só que o registro fica descobrível.
    */
    #[test]
    fn offer_controller_is_discoverable_by_name() {
        let mut registry = StateRegistry::new();
        registry.offer_controller("reactor_pressure_control", Rc::new(DummyController));
        assert!(registry.controller("reactor_pressure_control").is_some());
        assert_eq!(
            registry.controller_names().collect::<Vec<_>>(),
            vec!["reactor_pressure_control"]
        );
    }
}
