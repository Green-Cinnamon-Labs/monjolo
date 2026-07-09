// opcua_adapter.rs
//
// Adaptador OPC-UA genérico (ver docs/issue55_opcua_refactor/plan_refactor.md,
// seção 10): expõe a IoImage de uma Simulation via um servidor OPC-UA
// mínimo. Não sabe nada de TEP/química/planta específica — só enxerga os
// nomes de sinais que quem montou a Simulation já declarou via
// `Simulation::add_sensor`/`add_actuator`. A fronteira é: o framework expõe;
// quem monta a Simulation decide o que existe e como se chama.
//
// Requer a feature `opcua` — puxa async-opcua + tokio, pesados demais pra
// serem dependência default do resto do crate.
//
// Sensores viram nodes read-only, atualizados por push (`set_values`) a
// cada tick — nunca por `add_read_callback`, porque callbacks de leitura
// seriam chamados pela árvore de conexão do servidor, e nada aqui precisa
// disso: o valor já está pronto depois de cada `Simulation::run()`.
//
// Atuadores viram nodes writable com um `add_write_callback` de verdade —
// mas esse callback é `Send + Sync` (exigência do SimpleNodeManager) e
// `Simulation`/`IoImage` não são (guardam `Rc<RefCell<StateRegistry>>`,
// deliberadamente single-thread — plan_refactor.md, seção 3.9). Por isso o
// callback não toca em `Simulation` direto: só empurra `(nome, valor)` num
// canal (`tokio::sync::mpsc`, cujo `Sender` É Send/Sync). O lado de cá do
// canal é drenado no mesmo loop que chama `Simulation::run()`, aplicando os
// comandos via `simulation.io().write(...)` antes do próximo passo.

use std::time::Duration;

use opcua::crypto::SecurityPolicy;
use opcua::server::address_space::Variable;
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{simple_node_manager, SimpleNodeManager};
use opcua::server::ServerBuilder;
use opcua::types::{DataValue, MessageSecurityMode, NodeId, NumericRange, StatusCode};
use tokio::sync::mpsc;

use crate::simulation::Simulation;

const NAMESPACE_URI: &str = "urn:simulation-framework:opcua-adapter";

/** Sobe um servidor OPC-UA expondo todos os sinais já declarados na
`IoImage` da `simulation` recebida: um node read-only por
`io.sensor_names()`, um node writable por `io.actuator_names()`.

`endpoint` no formato `opc.tcp://<host>:<porta><path>`, ex.:
`"opc.tcp://0.0.0.0:4840/tep/server/"`.

Bloqueia até o servidor encerrar (erro fatal — não há shutdown gracioso
ainda). Move `simulation` pra dentro: dono exclusivo do loop de tick daqui
em diante.
*/
pub async fn serve(mut simulation: Simulation, endpoint: &str) -> Result<(), String> {
    let (host, port, path) = parse_endpoint(endpoint)?;

    let (server, handle) = ServerBuilder::new()
        .application_name("simulation-framework OPC-UA adapter")
        .application_uri(NAMESPACE_URI)
        .host(host)
        .port(port)
        .add_endpoint(
            "none",
            (
                path.as_str(),
                SecurityPolicy::None,
                MessageSecurityMode::None,
                &["ANONYMOUS"] as &[&str],
            ),
        )
        .discovery_urls(vec![path.clone()])
        .with_node_manager(simple_node_manager(
            NamespaceMetadata {
                namespace_uri: NAMESPACE_URI.to_owned(),
                ..Default::default()
            },
            "adapter",
        ))
        .trust_client_certs(true)
        .build()
        .map_err(|e| format!("falha ao construir o servidor OPC-UA: {e}"))?;

    let node_manager = handle
        .node_managers()
        .get_of_type::<SimpleNodeManager>()
        .ok_or_else(|| "SimpleNodeManager não encontrado".to_string())?;
    let ns = handle
        .get_namespace_index(NAMESPACE_URI)
        .ok_or_else(|| "namespace não registrado".to_string())?;

    let sensor_names: Vec<String> = simulation.io().sensor_names().map(str::to_owned).collect();
    let actuator_names: Vec<String> = simulation
        .io()
        .actuator_names()
        .map(str::to_owned)
        .collect();

    let (tx, mut rx) = mpsc::unbounded_channel::<(String, f64)>();

    let sensor_nodes: Vec<(NodeId, String)> = {
        let address_space = node_manager.address_space();
        let mut address_space = address_space.write();

        let folder_id = NodeId::new(ns, "signals");
        address_space.add_folder(
            &folder_id,
            "Signals",
            "Signals",
            &NodeId::objects_folder_id(),
        );

        let sensor_nodes: Vec<(NodeId, String)> = sensor_names
            .into_iter()
            .map(|name| {
                let node_id = NodeId::new(ns, name.clone());
                let _ = address_space.add_variables(
                    vec![Variable::new(&node_id, name.as_str(), name.as_str(), 0f64)],
                    &folder_id,
                );
                (node_id, name)
            })
            .collect();

        for name in actuator_names {
            let node_id = NodeId::new(ns, name.clone());
            let mut var = Variable::new(&node_id, name.as_str(), name.as_str(), 0f64);
            var.set_writable(true);
            let _ = address_space.add_variables(vec![var], &folder_id);

            let tx = tx.clone();
            let cb_name = name.clone();
            node_manager.inner().add_write_callback(
                node_id,
                move |data_value: DataValue, _range: &NumericRange| match data_value
                    .value
                    .as_ref()
                    .and_then(|v| v.as_f64())
                {
                    Some(value) => {
                        let _ = tx.send((cb_name.clone(), value));
                        StatusCode::Good
                    }
                    None => StatusCode::BadTypeMismatch,
                },
            );
        }

        sensor_nodes
    };

    let subscriptions = handle.subscriptions().clone();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                loop {
                    interval.tick().await;

                    while let Ok((name, value)) = rx.try_recv() {
                        let _ = simulation.io().write(&name, value);
                    }

                    simulation.run();

                    let updates: Vec<_> = sensor_nodes
                        .iter()
                        .map(|(node_id, name)| {
                            let value = simulation.io().read(name).unwrap_or(f64::NAN);
                            (node_id, None, DataValue::new_now(value))
                        })
                        .collect();

                    let _ = node_manager.set_values(&subscriptions, updates.into_iter());
                }
            });

            server.run().await
        })
        .await
        .map_err(|e| format!("servidor OPC-UA encerrou com erro: {e}"))
}

fn parse_endpoint(endpoint: &str) -> Result<(String, u16, String), String> {
    let rest = endpoint
        .strip_prefix("opc.tcp://")
        .ok_or_else(|| format!("endpoint '{endpoint}' precisa começar com opc.tcp://"))?;
    let (authority, raw_path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{raw_path}");
    let (host, port) = authority
        .split_once(':')
        .ok_or_else(|| format!("endpoint '{endpoint}' precisa de host:porta"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("porta inválida em '{endpoint}'"))?;
    Ok((host.to_string(), port, path))
}
