/* src/adapter/function.rs

Documentação em `docs/adapter-function.md`; testes em `tests/adapter_function.rs`.
*/

use std::panic::{self, AssertUnwindSafe};

use serde_json::{json, Map, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::model_function::ModelFunction;

pub struct FunctionServer {
    server: Server,
}

impl FunctionServer {
    pub fn bind(address: &str) -> Result<Self, String> {
        let server = Server::http(address)
            .map_err(|e| format!("falha ao abrir o servidor de função em '{address}': {e}"))?;
        Ok(Self { server })
    }

    pub fn local_address(&self) -> String {
        self.server.server_addr().to_string()
    }

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
