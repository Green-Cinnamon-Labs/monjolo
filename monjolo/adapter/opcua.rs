/** src/adapter/opcua.rs

Adaptador OPC-UA genérico: expõe sensores/atuadores via um servidor OPC-UA mínimo. Não sabe nada de
TEP/química/planta específica, nem de `StateRegistry` — só de `crate::runtime::Runtime`, o supervisor
persistente (issue #67) que troca atomicamente pra qual `PlantBinding` (sensores, atuadores-espelho,
canal de comando, `RuntimeControl`) está "ativo agora".

Requer a feature `opcua` — puxa async-opcua + tokio, pesados demais pra serem dependência default do
resto do crate.

NOTA (2026-09-09, issue #67): antes desta issue, `serve()` recebia `(sensors, actuators, commands,
control)` UMA vez e os capturava pra sempre — correto enquanto só existia uma `Simulation` pelo tempo
de vida inteiro do processo. Agora que `Runtime::reset()` descarta a `Simulation` atual e sobe outra
do zero, aqueles quatro valores mudam de identidade a cada reset (novos `Arc<dyn Sensor>`, novo
`Sender`, novo `Arc<RuntimeControl>`) — capturar UMA vez faria todo Read/Write/Method continuar
apontando pra uma planta morta depois do primeiro reset. Por isso todo callback abaixo (o loop de
push periódico, o write callback de atuador, os Methods de controle) chama `runtime.binding()`
FRESCO a cada execução, nunca guarda o `Arc<PlantBinding>` de uma vez pra outra. A ESTRUTURA de nodes
(quais NodeId existem) continua construída uma única vez, no boot — os nomes de sensor/atuador são
estáveis entre resets (mesmas chaves sempre), só o que está por trás de cada nome muda.

Sensores viram nodes read-only, atualizados por push (`set_values`) a cada tick, chamando
`sensor.read()` no `Arc<dyn Sensor>` que a leitura FRESCA de `runtime.binding()` devolver pro nome
daquele node — `Sensor` é `Send + Sync` de verdade, então atravessa sem bridge nenhuma.

Atuadores viram um único node por nome, writable E atualizado por push — as duas coisas. `Actuator`
em si NÃO atravessa — guarda `Proxy` (`Rc`-based), `!Send`/`!Sync` por construção. A ESCRITA (comando
entrando) vai pelo canal `commands` do `PlantBinding` ATUAL (lido fresco no momento do Write, não
capturado no registro do callback) pra Thread da planta viva, que drena e chama `actuator.write()`
localmente. A LEITURA (posição saindo) usa o `Sensor` espelho do `PlantBinding` atual, na mesma
chave.

`clock.t_h` é só mais um node read-only na lista de push — `RuntimeControl` implementa `Sensor`
direto (`fn read(&self) -> f64 { self.t_h() }`). `control.pause`/`control.resume`/`control.set_speed`
viram `Method` (`Call`), cada callback resolvendo `runtime.binding().control` fresco antes de agir.
`control.reset`/`control.shutdown` são dois Methods novos que chamam direto em `Runtime` (não em
`RuntimeControl` — não há uma instância de `RuntimeControl` que sobreviva a um reset pra chamar isso
nela) — cada um roda `Runtime::reset()`/`request_shutdown()` (bloqueantes) dentro do seu próprio
`std::thread::spawn`, pra nunca travar o runtime tokio de thread única deste adaptador.
*/

use std::sync::Arc;
use std::time::Duration;

use opcua::crypto::SecurityPolicy;
use opcua::server::address_space::{AccessLevel, MethodBuilder, Variable};
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{simple_node_manager, SimpleNodeManager};
use opcua::server::ServerBuilder;
use opcua::types::{
    DataTypeId, DataValue, MessageSecurityMode, NodeId, NumericRange, StatusCode, Variant,
};

use crate::runtime::Runtime;
use crate::sensor::Sensor;

const NAMESPACE_URI: &str = "urn:monjolo:opcua-adapter";
/* Tem que ser DIFERENTE de NAMESPACE_URI — `DiagnosticsNodeManager` (registrado automaticamente
pelo `async-opcua-server`, junto do "core", antes do nosso `SimpleNodeManager` na lista de node
managers) auto-registra um namespace usando `ServerInfo::application_uri` como URI, na sua própria
ordem de construção (2º, antes do nosso "adapter", 3º). Se essa URI for igual ao NAMESPACE_URI dos
NOSSOS nodes, o índice de namespace vira o MESMO pros dois managers, e `DiagnosticsNodeManager`
(construído primeiro) reivindica `owns_node()` pra ele — Read/Write nunca chegam a alcançar o
SimpleNodeManager de verdade, ficam presos em `BadNodeIdUnknown` (diagnostics não conhece os nossos
nodes). Browse não sofre disso — não filtra por `owns_node()`, tenta todo node manager pra todo
node, por isso "funciona" enquanto Read/Write silenciosamente não.
*/
const APPLICATION_URI: &str = "urn:monjolo:opcua-adapter:app";

/** Sobe um servidor OPC-UA: um node read-only por sensor, um node read-write por atuador, mais
`clock.t_h` (read-only) e cinco `Method` (`control.pause`/`resume`/`set_speed`/`reset`/`shutdown`) —
todos resolvidos contra `runtime.binding()` fresco a cada execução, nunca contra uma planta fixa (ver
nota no topo do arquivo).

`endpoint` no formato `opc.tcp://<host>:<porta><path>`, ex.: `"opc.tcp://0.0.0.0:4840/tep/server/"`.

Bloqueia até o servidor encerrar (normalmente: nunca de propósito — `control.shutdown` não fecha
este servidor TCP diretamente, para a planta e sinaliza `Runtime::wait_for_shutdown()`; é o
`main()` de quem monta o processo, retornando logo depois disso, que acaba encerrando esta thread
junto ao resto do processo — ver `Runtime::request_shutdown`).
*/
pub async fn serve(runtime: Arc<Runtime>, endpoint: &str) -> Result<(), String> {
    let (host, port, path) = parse_endpoint(endpoint)?;
    /* `discovery_urls` precisa de URL completa (`opc.tcp://host:porta/caminho`), não só o path —
    server.rs::base_endpoint() usa isso pra construir o `EndpointUrl` que devolve em
    GetEndpoints/FindServers; clientes que confiam nesse valor pra reconectar (UaExpert, não
    opcua-commander/conexão direta) recebem um endpoint inválido sem isso.
    */
    let full_url = format!("opc.tcp://{host}:{port}{path}");

    let (server, handle) = ServerBuilder::new()
        .application_name("monjolo OPC-UA adapter")
        .application_uri(APPLICATION_URI)
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
        .discovery_urls(vec![full_url])
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

    /* Só pra saber QUAIS nomes existem — a estrutura de nodes é construída uma única vez, aqui, a
    partir da planta que estiver ativa agora. Assume-se que todo reset produz o MESMO conjunto de
    chaves de sensor/atuador (mesmo binário, mesmo conjunto de `#[sensor(...)]`/`#[actuator(...)]`
    descobertos por `inventory`) — se isso um dia deixar de valer, a estrutura de nodes precisaria
    ser reconstruída a cada reset também, não só o valor por trás de cada um.
    */
    let initial = runtime.binding();

    let (sensor_nodes, actuator_nodes, clock_id): (Vec<(NodeId, String)>, Vec<(NodeId, String)>, NodeId) = {
        let address_space = node_manager.address_space();
        let mut address_space = address_space.write();

        let folder_id = NodeId::new(ns, "signals");
        address_space.add_folder(
            &folder_id,
            "Signals",
            "Signals",
            &NodeId::objects_folder_id(),
        );

        let sensor_nodes: Vec<(NodeId, String)> = initial
            .sensors
            .keys()
            .map(|name| (NodeId::new(ns, name.clone()), name.clone()))
            .collect();
        for (node_id, name) in &sensor_nodes {
            let _ = address_space.add_variables(
                vec![Variable::new(node_id, name.as_str(), name.as_str(), 0f64)],
                &folder_id,
            );
        }

        /* Um único node por atuador, read-write — a mesma NodeId é lida via o Sensor espelho E
        ganha o write callback (comando entrando). Não são dois nodes: seria ambíguo pro cliente
        OPC-UA (qual dos dois é "a válvula X"?) e a UI nem deixaria escrever no que parece ser um
        node read-only.
        */
        let actuator_nodes: Vec<(NodeId, String)> = initial
            .actuators
            .keys()
            .map(|name| (NodeId::new(ns, name.clone()), name.clone()))
            .collect();
        for (node_id, name) in &actuator_nodes {
            let mut var = Variable::new(node_id, name.as_str(), name.as_str(), 0f64);
            /* `set_writable()` só mexe em `access_level` (capacidade do SERVIDOR) — o serviço de
            Write valida contra `user_access_level` (capacidade do USUÁRIO autenticado), que
            `Variable::new()` inicializa só com `CURRENT_READ`. Sem isso, todo Write cai em
            `BadUserAccessDenied` mesmo com `access_level` liberado.
            */
            var.set_writable(true);
            var.set_user_access_level(AccessLevel::CURRENT_READ | AccessLevel::CURRENT_WRITE);
            let _ = address_space.add_variables(vec![var], &folder_id);

            let runtime_for_write = runtime.clone();
            let name_for_write = name.clone();
            node_manager.inner().add_write_callback(
                node_id.clone(),
                move |data_value: DataValue, _range: &NumericRange| match data_value
                    .value
                    .as_ref()
                    .and_then(|v| v.as_f64())
                {
                    Some(value) => {
                        /* Fresco a cada Write — o `Sender` de uma planta antiga (pré-reset) não
                        tem mais ninguém do outro lado pra receber; capturar um `Sender` fixo aqui
                        faria toda escrita depois de um reset se perder silenciosamente. */
                        let _ = runtime_for_write
                            .binding()
                            .commands
                            .send((name_for_write.clone(), value));
                        StatusCode::Good
                    }
                    None => StatusCode::BadTypeMismatch,
                },
            );
        }

        /* `RuntimeControl` implementa `Sensor` (runtime_control.rs) só pra expor `t_h` — entra na
        mesma lista de push que qualquer sensor de verdade, sem node manager/loop dedicado.
        */
        let clock_id = NodeId::new(ns, "clock.t_h");
        let _ = address_space.add_variables(
            vec![Variable::new(&clock_id, "clock.t_h", "clock.t_h", 0f64)],
            &folder_id,
        );

        /* Cinco Method (`Call`, não `Read`/`Write`). pause/resume/set_speed resolvem `binding()`
        fresco a cada chamada — sempre agem na planta viva, nunca numa instância descartada por um
        reset anterior. reset/shutdown chamam direto em `Runtime` (não em `RuntimeControl`: não há
        uma instância que sobreviva a um reset pra segurar esse estado) — cada um roda a chamada
        bloqueante do `Runtime` dentro do seu próprio `std::thread::spawn`, pra nunca travar o
        runtime tokio de thread única deste adaptador (`reset()` espera até 1 tick_interval pela
        Thread da planta antiga morrer + o tempo de subir uma nova).
        */
        let pause_id = NodeId::new(ns, "control.pause");
        MethodBuilder::new(&pause_id, "control.pause", "control.pause")
            .component_of(folder_id.clone())
            .insert(&mut *address_space);
        let runtime_for_pause = runtime.clone();
        node_manager
            .inner()
            .add_method_callback(pause_id, move |_args: &[Variant]| {
                runtime_for_pause.binding().control.pause();
                Ok(Vec::new())
            });

        let resume_id = NodeId::new(ns, "control.resume");
        MethodBuilder::new(&resume_id, "control.resume", "control.resume")
            .component_of(folder_id.clone())
            .insert(&mut *address_space);
        let runtime_for_resume = runtime.clone();
        node_manager
            .inner()
            .add_method_callback(resume_id, move |_args: &[Variant]| {
                runtime_for_resume.binding().control.resume();
                Ok(Vec::new())
            });

        let speed_id = NodeId::new(ns, "control.set_speed");
        let speed_args_id = NodeId::new(ns, "control.set_speed.InputArguments");
        MethodBuilder::new(&speed_id, "control.set_speed", "control.set_speed")
            .component_of(folder_id.clone())
            .input_args(
                &mut *address_space,
                &speed_args_id,
                &[("factor", DataTypeId::Double).into()],
            )
            .insert(&mut *address_space);
        let runtime_for_speed = runtime.clone();
        node_manager
            .inner()
            .add_method_callback(speed_id, move |args: &[Variant]| {
                let factor = args
                    .first()
                    .and_then(Variant::as_f64)
                    .ok_or(StatusCode::BadInvalidArgument)?;
                runtime_for_speed.binding().control.set_speed(factor);
                Ok(Vec::new())
            });

        let reset_id = NodeId::new(ns, "control.reset");
        MethodBuilder::new(&reset_id, "control.reset", "control.reset")
            .component_of(folder_id.clone())
            .insert(&mut *address_space);
        let runtime_for_reset = runtime.clone();
        node_manager
            .inner()
            .add_method_callback(reset_id, move |_args: &[Variant]| {
                let runtime_for_reset = runtime_for_reset.clone();
                std::thread::spawn(move || {
                    if let Err(err) = runtime_for_reset.reset() {
                        eprintln!("[adapter] control.reset falhou: {err}");
                    }
                });
                Ok(Vec::new())
            });

        let shutdown_id = NodeId::new(ns, "control.shutdown");
        MethodBuilder::new(&shutdown_id, "control.shutdown", "control.shutdown")
            .component_of(folder_id.clone())
            .insert(&mut *address_space);
        let runtime_for_shutdown = runtime.clone();
        node_manager
            .inner()
            .add_method_callback(shutdown_id, move |_args: &[Variant]| {
                let runtime_for_shutdown = runtime_for_shutdown.clone();
                std::thread::spawn(move || runtime_for_shutdown.request_shutdown());
                Ok(Vec::new())
            });

        (sensor_nodes, actuator_nodes, clock_id)
    };

    let subscriptions = handle.subscriptions().clone();

    let runtime_for_push = runtime.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;

            let binding = runtime_for_push.binding();
            let mut updates = Vec::with_capacity(sensor_nodes.len() + actuator_nodes.len() + 1);

            for (node_id, name) in &sensor_nodes {
                if let Some(sensor) = binding.sensors.get(name) {
                    updates.push((node_id, None, DataValue::new_now(sensor.read())));
                }
            }
            for (node_id, name) in &actuator_nodes {
                if let Some(shadow) = binding.actuators.get(name) {
                    updates.push((node_id, None, DataValue::new_now(shadow.read())));
                }
            }
            updates.push((&clock_id, None, DataValue::new_now(binding.control.read())));

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
