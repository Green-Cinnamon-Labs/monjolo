// src/adapter/opcua.rs
//
// Adaptador OPC-UA genérico (ver docs/issue55_opcua_refactor/plan_refactor.md,
// seção 10): expõe sensores/atuadores via um servidor OPC-UA mínimo. Não
// sabe nada de TEP/química/planta específica, nem de `Simulation`/
// `StateRegistry`/`IoImage` — só recebe nomes (`sensor_names`/
// `actuator_names`) e as duas pontes thread-safe que a "Thread da planta"
// já publica/consome (`SnapshotBus`/`CommandQueue`, ver
// drawio/dynamicModel.drawio, aba "arquitetura"). Quem chama essa função é
// `Simulation::run()`, nunca o usuário do framework direto.
//
// Requer a feature `opcua` — puxa async-opcua + tokio, pesados demais pra
// serem dependência default do resto do crate.
//
// Sensores viram nodes read-only, atualizados por push (`set_values`) a
// cada tick, lendo do `SnapshotBus` — nunca por `add_read_callback`, porque
// o valor já está publicado, não precisa ser computado sob demanda.
//
// Atuadores viram nodes writable com um `add_write_callback` de verdade —
// esse callback é `Fn(...) + Send + Sync + 'static` (exigência do
// SimpleNodeManager) e só toca o `CommandQueue` (que é Send+Sync de
// verdade, ao contrário do que tínhamos antes com `Simulation`/`IoImage`
// direto) — sem LocalSet/spawn_local, sem runtime current_thread: nada
// aqui é !Send, então roda no runtime tokio padrão.

use std::time::Duration;

use opcua::crypto::SecurityPolicy;
use opcua::server::address_space::Variable;
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{simple_node_manager, SimpleNodeManager};
use opcua::server::ServerBuilder;
use opcua::types::{DataValue, MessageSecurityMode, NodeId, NumericRange, StatusCode};

use crate::adapter::command_queue::CommandQueue;
use crate::adapter::snapshot_bus::SnapshotBus;

const NAMESPACE_URI: &str = "urn:simulation-framework:opcua-adapter";

/** Sobe um servidor OPC-UA: um node read-only por nome em `sensor_names`
(lido de `snapshot` a cada tick), um node writable por nome em
`actuator_names` (escrita empurrada em `commands`).

`endpoint` no formato `opc.tcp://<host>:<porta><path>`, ex.:
`"opc.tcp://0.0.0.0:4840/tep/server/"`.

Bloqueia até o servidor encerrar (erro fatal — não há shutdown gracioso
ainda).
*/
pub async fn serve(
    sensor_names: Vec<String>,
    actuator_names: Vec<String>,
    snapshot: SnapshotBus,
    commands: CommandQueue,
    endpoint: &str,
) -> Result<(), String> {
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

            let commands = commands.clone();
            let cb_name = name.clone();
            node_manager.inner().add_write_callback(
                node_id,
                move |data_value: DataValue, _range: &NumericRange| match data_value
                    .value
                    .as_ref()
                    .and_then(|v| v.as_f64())
                {
                    Some(value) => {
                        commands.write(&cb_name, value);
                        StatusCode::Good
                    }
                    None => StatusCode::BadTypeMismatch,
                },
            );
        }

        sensor_nodes
    };

    let subscriptions = handle.subscriptions().clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;

            let updates: Vec<_> = sensor_nodes
                .iter()
                .map(|(node_id, name)| {
                    let value = snapshot.read(name).unwrap_or(f64::NAN);
                    (node_id, None, DataValue::new_now(value))
                })
                .collect();

            let _ = node_manager.set_values(&subscriptions, updates.into_iter());
        }
    });

    server
        .run()
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
