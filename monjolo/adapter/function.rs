/** src/adapter/function.rs

Adaptador de função (spec-tennessee-eastman#94): serve um `ModelFunction` por HTTP — o
`StateRegistry` de um modelo, exposto pra quem quer avaliá-lo num ponto de fora do processo (um
otimizador, por exemplo). Não sabe nada de TEP nem de otimização: só de chaves, valores e avaliar.

Requer a feature `function` (`tiny_http` + `serde_json`).

Três rotas:

- `GET /healthz` — `{"status":"ok","keys":N,"states":M}`.
- `GET /keys` — `{"keys":[{"key":"...","value":1.0,"state":true}, ...]}`: toda chave do registry,
  com o valor no ponto base e a marca de quais são estados integráveis.
- `POST /evaluate` — corpo `{"read":["k1","k2"],"points":[{"k3":1.0},{"k3":2.0}]}`; resposta
  `{"read":["k1","k2"],"values":[[..],[..]]}`, uma linha por ponto, na ordem de `read`. Cada ponto
  é o conjunto de chaves a escrever por cima do ponto base (`ModelFunction::evaluate`). Vários
  pontos por chamada, porque um gradiente por diferenças finitas pede dezenas de avaliações.

Síncrono e de uma thread só, de propósito: `ModelFunction` é `Rc`-based (`!Send`), então quem
avalia tem que ser a mesma thread que montou o modelo. Um laço que recebe, avalia e responde nessa
thread dispensa canal e runtime assíncrono; uma avaliação leva microssegundos.

Valor não finito (NaN, infinito) não existe em JSON: sai como `null`.
*/

use std::panic::{self, AssertUnwindSafe};

use serde_json::{json, Map, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::model_function::ModelFunction;

pub struct FunctionServer {
    server: Server,
}

impl FunctionServer {
    /** `address` no formato `host:porta`, ex.: `"127.0.0.1:8091"`. Porta `0` deixa o sistema
    escolher uma livre — ver `local_address()`.
    */
    pub fn bind(address: &str) -> Result<Self, String> {
        let server = Server::http(address)
            .map_err(|e| format!("falha ao abrir o servidor de função em '{address}': {e}"))?;
        Ok(Self { server })
    }

    /* O endereço em que o servidor ficou de fato, `host:porta`. */
    pub fn local_address(&self) -> String {
        self.server.server_addr().to_string()
    }

    /** Atende requisições pra sempre, na thread de quem chama — a mesma que montou `function`. */
    pub fn serve(&self, function: &ModelFunction) -> ! {
        loop {
            match self.server.recv() {
                Ok(request) => handle(request, function),
                Err(err) => eprintln!("[function] erro ao receber requisição: {err}"),
            }
        }
    }
}

fn handle(mut request: Request, function: &ModelFunction) {
    let path = request.url().split('?').next().unwrap_or("").to_string();
    let (status, body) = match (request.method(), path.as_str()) {
        (Method::Get, "/healthz") => (200, healthz(function)),
        (Method::Get, "/keys") => (200, keys(function)),
        (Method::Post, "/evaluate") => {
            let mut text = String::new();
            match request.as_reader().read_to_string(&mut text) {
                Ok(_) => match evaluate(&text, function) {
                    Ok(body) => (200, body),
                    Err((status, message)) => (status, json!({ "error": message })),
                },
                Err(err) => (400, json!({ "error": format!("corpo ilegível: {err}") })),
            }
        }
        (_, "/healthz") | (_, "/keys") | (_, "/evaluate") => {
            (405, json!({ "error": "método não suportado nesta rota" }))
        }
        _ => (404, json!({ "error": format!("rota desconhecida: {path}") })),
    };

    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("cabeçalho estático válido");
    let response = Response::from_string(body.to_string()).with_status_code(status).with_header(header);
    if let Err(err) = request.respond(response) {
        eprintln!("[function] erro ao responder: {err}");
    }
}

fn healthz(function: &ModelFunction) -> Value {
    json!({
        "status": "ok",
        "keys": function.keys().len(),
        "states": function.state_keys().len(),
    })
}

fn keys(function: &ModelFunction) -> Value {
    let states = function.state_keys();
    let entries: Vec<Value> = function
        .keys()
        .iter()
        .map(|key| {
            json!({
                "key": key,
                "value": function.base(key),
                "state": states.contains(key),
            })
        })
        .collect();
    json!({ "keys": entries })
}

/* `Err((status, mensagem))`: 400 pro que é culpa de quem chamou (JSON malformado, chave que não
existe), 500 se a própria avaliação entrar em pânico num ponto.
*/
fn evaluate(text: &str, function: &ModelFunction) -> Result<Value, (u16, String)> {
    let bad_request = |message: String| (400, message);

    let body: Value = serde_json::from_str(text).map_err(|e| bad_request(format!("JSON inválido: {e}")))?;
    let read: Vec<&str> = body
        .get("read")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("falta `read`: a lista de chaves a ler".to_string()))?
        .iter()
        .map(|key| key.as_str().ok_or_else(|| bad_request("`read` só aceita texto".to_string())))
        .collect::<Result<_, _>>()?;
    let points: &Vec<Value> = body
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("falta `points`: a lista de pontos a avaliar".to_string()))?;

    let mut values = Vec::with_capacity(points.len());
    for (position, point) in points.iter().enumerate() {
        let writes = writes_of(point).map_err(|message| bad_request(format!("ponto {position}: {message}")))?;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| function.evaluate(&writes, &read)));
        match outcome {
            Ok(Ok(row)) => values.push(row),
            Ok(Err(message)) => return Err(bad_request(format!("ponto {position}: {message}"))),
            Err(_) => return Err((500, format!("ponto {position}: a avaliação do modelo entrou em pânico"))),
        }
    }

    Ok(json!({ "read": read, "values": values }))
}

fn writes_of(point: &Value) -> Result<Vec<(&str, f64)>, String> {
    let object: &Map<String, Value> =
        point.as_object().ok_or_else(|| "cada ponto é um objeto {chave: valor}".to_string())?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_f64()
                .map(|number| (key.as_str(), number))
                .ok_or_else(|| format!("o valor de '{key}' não é um número"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_model::DynamicModel;
    use crate::snapshot::Snapshot;
    use crate::state_registry::{Proxy, StateRegistry};
    use std::io::{Read, Write};
    use std::net::TcpStream;

    const RATE: f64 = 0.5;

    /* Mesmo modelo de brinquedo de `model_function::tests`: dv/dt = -RATE·v e `toy.double = 2v`. */
    struct Decay {
        value: Proxy,
        derivative: Proxy,
        double: Proxy,
    }

    impl DynamicModel for Decay {
        fn evaluate(&self) {
            let value = self.value.get();
            self.derivative.set(-RATE * value);
            self.double.set(2.0 * value);
        }

        fn state_keys(&self) -> Vec<String> {
            vec!["toy.value".to_string()]
        }
    }

    /* Sobe o servidor numa porta livre, numa thread própria (o `ModelFunction` nasce e morre nela),
    e devolve o endereço. A thread fica servindo até o processo de teste acabar.
    */
    fn start_server() -> String {
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let config = Snapshot::from_pairs(&[("toy.value", 10.0)]);
            let function = ModelFunction::new(&config, |registry: &mut StateRegistry, config: &Snapshot| {
                let (offered, _) =
                    registry.subscribe(&["toy.value", "toy.value.derivative", "toy.double"], &[]);
                offered[0].set(config.get("toy.value").unwrap_or(0.0));
                Decay {
                    value: offered[0].clone(),
                    derivative: offered[1].clone(),
                    double: offered[2].clone(),
                }
            })
            .expect("Decay é uma função do ponto");
            let server = FunctionServer::bind("127.0.0.1:0").expect("porta livre");
            address_tx.send(server.local_address()).unwrap();
            server.serve(&function);
        });
        address_rx.recv().expect("o servidor deveria informar o endereço")
    }

    /* Cliente HTTP mínimo, só pra não puxar uma dependência de teste: uma requisição por conexão. */
    fn request(address: &str, method: &str, path: &str, body: &str) -> (u16, Value) {
        let mut stream = TcpStream::connect(address).expect("conexão com o servidor de teste");
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
            Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        let status: u16 = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        let payload = response.split("\r\n\r\n").nth(1).unwrap_or("");
        (status, serde_json::from_str(payload).unwrap_or(Value::Null))
    }

    #[test]
    fn healthz_reports_the_size_of_the_model() {
        let address = start_server();

        let (status, body) = request(&address, "GET", "/healthz", "");

        assert_eq!(status, 200);
        assert_eq!(body, json!({ "status": "ok", "keys": 3, "states": 1 }));
    }

    #[test]
    fn keys_lists_every_key_with_base_value_and_state_mark() {
        let address = start_server();

        let (status, body) = request(&address, "GET", "/keys", "");

        assert_eq!(status, 200);
        assert_eq!(
            body,
            json!({ "keys": [
                { "key": "toy.double", "value": 20.0, "state": false },
                { "key": "toy.value", "value": 10.0, "state": true },
                { "key": "toy.value.derivative", "value": -5.0, "state": false },
            ]})
        );
    }

    #[test]
    fn evaluate_returns_one_row_per_point_in_the_order_of_read() {
        let address = start_server();

        let (status, body) = request(
            &address,
            "POST",
            "/evaluate",
            r#"{"read":["toy.value.derivative","toy.double"],"points":[{"toy.value":3.0},{},{"toy.value":8.0}]}"#,
        );

        assert_eq!(status, 200);
        assert_eq!(
            body,
            json!({
                "read": ["toy.value.derivative", "toy.double"],
                "values": [[-1.5, 6.0], [-5.0, 20.0], [-4.0, 16.0]],
            })
        );
    }

    /* O teste de memória pelo HTTP: A, B, A dentro de uma mesma chamada e em chamadas separadas. */
    #[test]
    fn point_a_gives_the_same_result_before_and_after_point_b() {
        let address = start_server();
        let point = |value: f64| format!(r#"{{"read":["toy.value.derivative","toy.double"],"points":[{{"toy.value":{value}}}]}}"#);

        let (_, together) = request(
            &address,
            "POST",
            "/evaluate",
            r#"{"read":["toy.value.derivative","toy.double"],"points":[{"toy.value":0.1},{"toy.value":700.0},{"toy.value":0.1}]}"#,
        );
        assert_eq!(together["values"][0], together["values"][2]);

        let (_, first) = request(&address, "POST", "/evaluate", &point(0.1));
        let _ = request(&address, "POST", "/evaluate", &point(700.0));
        let (_, again) = request(&address, "POST", "/evaluate", &point(0.1));
        assert_eq!(first, again);
        assert_eq!(first["values"][0], together["values"][0]);
    }

    /* O número que volta é exatamente o f64 calculado: 0.1 * -0.5 não é representável e ainda assim
    faz a ida e a volta por JSON sem perder o último bit.
    */
    #[test]
    fn numbers_survive_the_round_trip_through_json_bit_for_bit() {
        let address = start_server();
        let written = 0.1_f64 + 0.2_f64;

        let (_, body) = request(
            &address,
            "POST",
            "/evaluate",
            &format!(r#"{{"read":["toy.value","toy.value.derivative"],"points":[{{"toy.value":{written:?}}}]}}"#),
        );

        let row = &body["values"][0];
        assert_eq!(row[0].as_f64().unwrap().to_bits(), written.to_bits());
        assert_eq!(row[1].as_f64().unwrap().to_bits(), (-RATE * written).to_bits());
    }

    #[test]
    fn caller_mistakes_are_400_and_name_the_problem() {
        let address = start_server();

        let (status, body) = request(&address, "POST", "/evaluate", r#"{"read":["toy.missing"],"points":[{}]}"#);
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("toy.missing"));

        let (status, body) = request(&address, "POST", "/evaluate", r#"{"read":[],"points":[{"toy.value":"x"}]}"#);
        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("toy.value"));

        let (status, _) = request(&address, "POST", "/evaluate", "isto não é JSON");
        assert_eq!(status, 400);

        let (status, _) = request(&address, "POST", "/evaluate", r#"{"points":[{}]}"#);
        assert_eq!(status, 400);
    }

    #[test]
    fn unknown_routes_and_wrong_methods_are_rejected() {
        let address = start_server();

        assert_eq!(request(&address, "GET", "/nope", "").0, 404);
        assert_eq!(request(&address, "GET", "/evaluate", "").0, 405);
        assert_eq!(request(&address, "POST", "/keys", "{}").0, 405);
    }
}
