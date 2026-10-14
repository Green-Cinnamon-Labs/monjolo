/* tests/adapter_function.rs

Prova `monjolo::adapter::function::FunctionServer` (spec-tennessee-eastman#94) por HTTP de verdade:
cada teste sobe o servidor numa porta livre e fala com ele por um `TcpStream`. As rotas e os erros
estão descritos em `docs/adapter-function.md`. Só compila com a feature `function` (ver `[[test]]`
no `Cargo.toml`).
*/

use std::io::{Read, Write};
use std::net::TcpStream;

use monjolo::adapter::function::FunctionServer;
use monjolo::dynamic_model::DynamicModel;
use monjolo::model_function::ModelFunction;
use monjolo::snapshot::Snapshot;
use monjolo::state_registry::{Proxy, StateRegistry};
use serde_json::{json, Value};

const RATE: f64 = 0.5;

/* Mesmo modelo de brinquedo de `tests/model_function.rs`: dv/dt = -RATE·v e `toy.double = 2v`. */
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

/* Sobe o servidor numa porta livre, numa thread própria (o `ModelFunction` nasce e morre nela), e
devolve o endereço. A thread fica servindo até o processo de teste acabar.
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

/* O número que volta é exatamente o f64 calculado: 0.1 * -0.5 não é representável e ainda assim faz
a ida e a volta por JSON sem perder o último bit.
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
