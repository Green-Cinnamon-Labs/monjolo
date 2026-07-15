/**monjolo/state_registry.rs

StateRegistry (ver docs/issue55_opcua_refactor/plan_refactor.md, seções 1.3,
6 e 7). Guarda dois mundos, sempre distintos:
  - CurrentState (`current_state`) — o estado real, confirmado, persistido.
    Compartilhável entre threads (Art. 1.3 §1º do plano legislativo): é o
    "último estado físico confirmado" — a Thread do Adaptador (e, no
    futuro, um Controlador) lê direto daqui, nunca de EvaluationState.
  - EvaluationState (`evaluation_state`) — a cópia de trabalho onde todo
    Proxy lê/escreve durante uma rodada de avaliação. Pode conter valores
    "hipotéticos" (chute intermediário de um solver iterativo) até alguém
    decidir que aquela rodada está ok. Thread-local — só a Thread da planta
    toca, sem lock, sem sincronização: `Rc<RefCell<Vec<Cell<f64>>>>`.
`commit()` é o commit EvaluationState -> CurrentState — mecânico, só copia,
mais o avanço de `generation` (Art. 3.6.2). A decisão de QUANDO chamar (ex.:
depois que um passo do Integrator convergiu) não é do StateRegistry, é de
quem orquestra a simulação.
*/
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

/**Uma entrada nomeada: nome semântico + valor, numa posição (implícita pelo
lugar no `Vec` que a contém — não redeclarada aqui).

`StateSlot` NÃO é o buffer quente de leitura/escrita — esse papel é do
`current_state`/`evaluation_state` internos, `Vec<Cell<f64>>` puro, sem nome
nenhum embutido. `StateSlot` só existe pra reconstrução sob demanda (ver
`StateRegistry::snapshot()`): metadado/catálogo pra inspeção, debug, listagem
de sinais ou exportação nomeada — nunca o caminho por onde `Proxy`/`ReadProxy`
leem ou escrevem. Resolver `key -> posição` de verdade, em tempo real, é
sempre trabalho do `index: HashMap<String, usize>`, nunca de vasculhar
`Vec<StateSlot>`.

Invariante: as posições são append-only. Uma vez que um slot é registrado,
sua posição nunca muda nem é reaproveitada — o que permite resolver uma
`key` para uma posição UMA ÚNICA VEZ e confiar nessa posição para sempre.
*/
pub struct StateSlot {
    pub key: String,
    pub value: f64,
}

/** Handle autossuficiente pra uma posição no buffer de avaliação — carrega o
buffer compartilhado (`Rc<RefCell<Vec<Cell<f64>>>>`) e o índice (`Rc<Cell<usize>>`)
juntos, então `get()`/`set()` não precisam de nada externo passado por
parâmetro. Nasce sem resolução (`index = usize::MAX`);
`StateRegistry::resolve()` escreve o índice real nele. Todo clone de um
`Proxy` aponta pro mesmo `Cell`, então resolver uma vez basta — o componente
guarda seu clone desde a inscrição e nunca mais precisa perguntar pelo nome
de novo, nem receber o buffer de fora em cada `evaluate()`.

Agnóstico a se o valor por trás é "hipotético" (chute intermediário de um
solver iterativo) ou "real" (convergido) — só endereça a posição.
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

/** Estado confirmado de verdade, por trás de `Arc<RwLock<...>>` — o "último
estado físico confirmado da planta" (Art. 1.3 §1º do plano legislativo).
`generation` avança exatamente uma vez por `commit()` (nunca por escrita
individual): é o que permite a um leitor de fora saber se dois valores lidos
vieram do mesmo tick confirmado ou de ticks diferentes, sem comparar os
valores em si — usado por `Sensor` (Art. 3.6.6) pra cache de idempotência
de `SensorBehavior`.
*/
struct CurrentState {
    generation: u64,
    values: Vec<f64>,
}

/** Handle resolvido-uma-vez sobre `CurrentState` — a contraparte de `Proxy`
só-leitura. Estruturalmente parecido (buffer + índice), mas um tipo à parte
de propósito: `Proxy` pode endereçar `EvaluationState`, que pode conter valor
hipotético de um solver iterativo em andamento (seção 7.2 do plano);
`ReadProxy` só existe sobre `CurrentState`, sempre o último valor confirmado.
Misturar os dois tipos não compila — é assim que essa garantia vira uma
propriedade do tipo, não uma regra de disciplina de quem usa.

Diferente de `Proxy`, nasce já resolvido: `ReadProxy` só é criado depois que
`StateRegistry::resolve()` já rodou (seção 6.3) e a chave, portanto, já existe
— não há segunda fase de resolução, e não existe `set()`: quem lê
`CurrentState` nunca deveria escrever nele por fora do `commit()` do
`StateRegistry`.

`buffer: Arc<RwLock<...>>`, não `Rc<RefCell<...>>`: ao contrário de `Proxy`
(thread-local, sobre `EvaluationState`), `ReadProxy` precisa atravessar
thread — é o que `Sensor` usa (Art. 3.6.2/11.9), e `Sensor` pode ser
compartilhado (`Arc<Sensor>`) entre a Thread da planta, a Thread do
Adaptador e um futuro Controlador.
*/
#[derive(Clone)]
pub struct ReadProxy {
    buffer: Arc<RwLock<CurrentState>>,
    index: usize,
}

impl ReadProxy {
    /// Leitura simples, sem versão — quem só quer o valor bruto confirmado.
    pub fn get(&self) -> f64 {
        self.buffer
            .read()
            .expect("CurrentState: lock envenenado")
            .values[self.index]
    }

    /** Leitura com `generation` — `(geração do CurrentState no momento da
    leitura, valor)`, resolvidos sob o mesmo lock (nunca podem divergir
    entre si). Usado por `Sensor::read()` (Art. 3.6.6) pra decidir se
    `SensorBehavior` precisa rodar de novo ou se o cache do tick ainda vale.
    */
    pub fn get_versioned(&self) -> (u64, f64) {
        let guard = self.buffer.read().expect("CurrentState: lock envenenado");
        (guard.generation, guard.values[self.index])
    }
}

pub struct StateRegistry {
    /** Buffer do estado confirmado (CurrentState, seção 1.3 do plano) —
    `Arc<RwLock<CurrentState>>`, escrito de uma vez só por `commit()`
    (nunca célula-a-célula): um único `write()` cobre valores E geração na
    mesma seção crítica, então quem lê nunca vê os dois dessincronizados.
    É sobre este buffer que `ReadProxy` resolve, uma vez, a posição que vai
    ler pra sempre. Sem nome embutido — nome é só em `index`; ver
    `snapshot()` pra reconstrução nomeada sob demanda.
    */
    current_state: Arc<RwLock<CurrentState>>,

    /** Buffer de trabalho de uma rodada de avaliação (seção 8 do plano).
     Compartilhado com todo `Proxy` já emitido — por isso `Rc<RefCell<_>>`
     (a lista cresce durante subscribe(), então precisa de mutabilidade;
     `Cell` por elemento é o que permite `evaluate()` escrever com `&self`).
    */
    evaluation_state: Rc<RefCell<Vec<Cell<f64>>>>,

    /// nome semântico -> posição em `evaluation_state`, preenchido conforme os outputs vão sendo oferecidos em subscribe().
    index: HashMap<String, usize>,

    /// Inputs declarados em subscribe(), ainda não resolvidos. resolve() esvazia essa lista, escrevendo a posição real em cada Proxy.
    pending_requests: Vec<(String, Proxy)>,
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
        }
    }

    /** Garante que `current_state` tenha, no mínimo, o tamanho de
     `evaluation_state` — só cresce, nunca encolhe (mesma invariante
     append-only da seção 5.2 do plano). Chamado em `resolve()` (pra
     `ReadProxy` já nascer endereçando uma posição válida, mesmo antes do
     primeiro `commit()`) e em `commit()` (defensivo, custo ~zero depois
     da primeira vez). Não avança `generation` — não é commit de valor
     nenhum, só reserva espaço.
    */
    fn ensure_current_capacity(&self) {
        let len = self.evaluation_state.borrow().len();
        let mut cur = self.current_state.write().expect("CurrentState: lock envenenado");
        while cur.values.len() < len {
            cur.values.push(0.0);
        }
    }

    /// Único jeito de obter um StateRegistry — não existe construtor público
    /// que devolva um valor solto. `shared()` sempre embrulha em `Rc<RefCell<_>>`,
    /// então todo `DynamicModel` que se inscreve guarda um clone do mesmo `Rc`
    /// (barato — só incrementa o contador de referência), apontando pra a
    /// mesma instância. Isso é o que faz dele um singleton de fato: não é uma
    /// única instância *global*, é uma única instância *por simulação*,
    /// garantida pelo tipo — não por disciplina de quem usa.
    pub fn shared() -> Rc<RefCell<StateRegistry>> {
        Rc::new(RefCell::new(Self::new()))
    }

    /** [REVISADO] | Um DynamicModel se inscreve: `offers` são os nomes dos slots que ele
    próprio provê (reservados e resolvidos na hora — a posição já é
    conhecida no momento em que a posição é criada); `needs` são as chaves
    de outros componentes que ele vai ler (devolvidas como Proxy NÃO
    resolvido — só ganham posição real em resolve()). Não importa a ordem
    de inscrição entre quem oferece e quem pede.
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

    /// Roda uma única vez, depois que todo mundo já se inscreveu. Resolve
    /// cada input pendente contra a posição já conhecida (de quem ofereceu
    /// aquele nome). Se algum input não tiver provedor, é erro — o resto
    /// pode ter ficado parcialmente resolvido, então não adianta continuar
    /// rodando a simulação depois disso falhar.
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
        self.ensure_current_capacity();
        Ok(())
    }

    /** Lê o valor já commitado de uma chave em CurrentState — leitura
    pontual por string, útil pra debug/inspeção avulsa. Nunca durante
    evaluate(), só depois que um passo já fechou. `Sensor` não deve usar
    isso no caminho quente — ver `read_proxy()`. None se a chave não existe
    ou se nenhum commit() rodou ainda.
    */
    pub fn read(&self, key: &str) -> Option<f64> {
        let idx = *self.index.get(key)?;
        let cur = self.current_state.read().expect("CurrentState: lock envenenado");
        cur.values.get(idx).copied()
    }

    /** Resolve uma chave, uma vez, contra `CurrentState` e devolve um
    `ReadProxy`. Só deve ser chamado depois que todo `DynamicModel` já se
    inscreveu (`subscribe()`) e `resolve()` geral já rodou — a chave precisa
    já existir em `index`; não há segunda fase de resolução como em `Proxy`.
    None se a chave não existir nesse momento (erro de configuração: sensor
    apontando pra algo que nenhum componente oferece).
    */
    pub fn read_proxy(&self, key: &str) -> Option<ReadProxy> {
        let idx = *self.index.get(key)?;
        Some(ReadProxy {
            buffer: self.current_state.clone(),
            index: idx,
        })
    }

    /** Foto nomeada do CurrentState — reconstrói `Vec<StateSlot>` sob
    demanda a partir de `index` + o buffer atual. Não é o armazenamento
    principal (esse é `current_state`, um `Vec<f64>` cru por trás do
    `RwLock`); é metadado/catálogo pra inspeção, debug, listagem de sinais
    ou exportação — não o caminho quente de leitura/escrita.
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

    /** Commit EvaluationState -> CurrentState (Art. 3.6.2 do plano
    legislativo): um único `write()` lock cobre a cópia inteira e o avanço
    de `generation` — nunca célula-a-célula. É esse "uma vez por tick, tudo
    junto" que dá a `current_state` a propriedade de "último estado físico
    confirmado": ninguém de fora consegue observar uma mistura de valores
    de ticks diferentes entre variáveis distintas, nem uma `generation`
    que já avançou mas com valores que ainda não. Não decide nada sobre SE
    deve commitar — só copia o que está lá no momento em que é chamado.
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

