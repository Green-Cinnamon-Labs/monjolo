/**simulation-framework/state_registry.rs

StateRegistry (ver docs/issue55_opcua_refactor/plan_refactor.md, seções 1.3,
6 e 7). Guarda dois mundos, sempre distintos:
  - CurrentState (`current_state`) — o estado real, confirmado, persistido.
  - EvaluationState (`evaluation_state`) — a cópia de trabalho onde todo
    Proxy lê/escreve durante uma rodada de avaliação. Pode conter valores
    "hipotéticos" (chute intermediário de um solver iterativo) até alguém
    decidir que aquela rodada está ok.
`commit()` é o commit EvaluationState -> CurrentState — mecânico, só copia. A
decisão de QUANDO chamar (ex.: depois que um passo do Integrator convergiu)
não é do StateRegistry, é de quem orquestra a simulação.
*/
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/**Uma entrada nomeada de um vetor: nome semântico + valor. A posição de um
 slot dentro do `Vec` que o contém É o seu índice — não é redeclarada aqui.

 Invariante: essas listas são append-only. Uma vez que um slot é registrado,
 sua posição nunca muda nem é reaproveitada. Isso é o que permite um
 consumidor resolver uma `key` para uma posição UMA ÚNICA VEZ e confiar
 nessa posição para sempre.
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
        Self { buffer, index: Rc::new(Cell::new(index)) }
    }

    fn unresolved(buffer: Rc<RefCell<Vec<Cell<f64>>>>) -> Self {
        Self { buffer, index: Rc::new(Cell::new(usize::MAX)) }
    }

    fn index(&self) -> usize {
        let idx = self.index.get();
        debug_assert!(idx != usize::MAX, "Proxy usado antes de StateRegistry::resolve()");
        idx
    }

    pub fn get(&self) -> f64 {
        self.buffer.borrow()[self.index()].get()
    }

    pub fn set(&self, value: f64) {
        self.buffer.borrow()[self.index()].set(value);
    }
}

pub struct StateRegistry {
    /// Estado oficial/persistido. `value` de cada slot aqui é o valor já
    /// confirmado do modelo.
    pub current_state: Vec<StateSlot>,

    /// Buffer de trabalho de uma rodada de avaliação (seção 8 do plano).
    /// Compartilhado com todo `Proxy` já emitido — por isso `Rc<RefCell<_>>`
    /// (a lista cresce durante subscribe(), então precisa de mutabilidade;
    /// `Cell` por elemento é o que permite `evaluate()` escrever com `&self`).
    evaluation_state: Rc<RefCell<Vec<Cell<f64>>>>,

    /// nome semântico -> posição em `evaluation_state`, preenchido conforme
    /// os outputs vão sendo oferecidos em subscribe().
    index: HashMap<String, usize>,

    /// Inputs declarados em subscribe(), ainda não resolvidos. resolve()
    /// esvazia essa lista, escrevendo a posição real em cada Proxy.
    pending_requests: Vec<(String, Proxy)>,
}

impl StateRegistry {
    fn new() -> Self {
        Self {
            current_state: Vec::new(),
            evaluation_state: Rc::new(RefCell::new(Vec::new())),
            index: HashMap::new(),
            pending_requests: Vec::new(),
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

    /// Um DynamicModel se inscreve: `offers` são os nomes dos slots que ele
    /// próprio provê (reservados e resolvidos na hora — a posição já é
    /// conhecida no momento em que a posição é criada); `needs` são as chaves
    /// de outros componentes que ele vai ler (devolvidas como Proxy NÃO
    /// resolvido — só ganham posição real em resolve()). Não importa a ordem
    /// de inscrição entre quem oferece e quem pede.
    pub fn subscribe(&mut self, offers: &[&str], needs: &[&str]) -> (Vec<Proxy>, Vec<Proxy>) {
        let offered = offers.iter().map(|&key| {
            let idx = self.evaluation_state.borrow().len();
            self.evaluation_state.borrow_mut().push(Cell::new(0.0));
            self.index.insert(key.to_string(), idx);
            Proxy::resolved(self.evaluation_state.clone(), idx)
        }).collect();

        let requested = needs.iter().map(|&key| {
            let proxy = Proxy::unresolved(self.evaluation_state.clone());
            self.pending_requests.push((key.to_string(), proxy.clone()));
            proxy
        }).collect();

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
                None => return Err(format!(
                    "input '{key}' declarado em subscribe() mas nenhum componente oferece esse slot"
                )),
            }
        }
        Ok(())
    }

    /// Commit EvaluationState -> CurrentState: reconstrói `current_state`
    /// inteiro a partir do buffer de trabalho atual, usando `index` pra
    /// recuperar o nome de cada posição. Não decide nada sobre SE deve
    /// commitar — só copia o que está lá no momento em que é chamado.
    pub fn commit(&mut self) {
        let buf = self.evaluation_state.borrow();
        let mut slots: Vec<StateSlot> = (0..buf.len())
            .map(|_| StateSlot { key: String::new(), value: 0.0 })
            .collect();
        for (key, &idx) in &self.index {
            slots[idx] = StateSlot { key: key.clone(), value: buf[idx].get() };
        }
        drop(buf);
        self.current_state = slots;
    }
}
