/** src/adapter/opcua.rs

Adaptador OPC-UA genérico: expõe sensores/atuadores via um servidor OPC-UA mínimo. Não sabe nada de
TEP/química/planta específica, nem de `Simulation`/`StateRegistry` — só recebe um catálogo de
`Arc<dyn Sensor>` (leitura, por nome) e uma lista de nomes de atuador, que a "Thread da planta" já
resolveu. Quem chama essa função é `Simulation::run()` (`spawn_plant_thread`), nunca o usuário do
framework direto.

Requer a feature `opcua` — puxa async-opcua + tokio, pesados demais pra serem dependência default do
resto do crate.

Sensores viram nodes read-only, atualizados por push (`set_values`) a cada tick, chamando
`sensor.read()` direto em cada `Arc<dyn Sensor>` — o mesmo objeto catalogado em `StateRegistry`,
compartilhado (não copiado). `Sensor` é `Send + Sync` de verdade (`ReadProxy` usa `Arc<AtomicUsize>`),
então atravessa pra esta thread sem bridge nenhuma: `Sensor::read()` já garante, sozinho, que duas
leituras dentro da mesma `generation` de `CurrentState` devolvem o mesmo valor.

Atuadores viram um único node por nome, writable E atualizado por push — as duas coisas. `Actuator`
em si NÃO atravessa — guarda `Proxy` (`Rc`-based), `!Send`/`!Sync` por construção, e isso não muda
aqui (mudar seria tocar o caminho mais quente do framework, lido/escrito em todo sub-passo do RK4).
A ESCRITA (comando entrando) continua pelo canal: cada node manda `(nome, valor)` por `commands` —
um `std::sync::mpsc::Sender` clonado por node — pra Thread da planta, que drena e chama
`actuator.write()` localmente, sem que nenhum `Rc` cruze a fronteira. A LEITURA (posição saindo) usa
o mesmo truque de sempre: quem chama `serve()` (`Simulation::spawn_adapter_thread`) constrói um
`Sensor` "espelho" na MESMA chave pra cada atuador — `Sensor` só lê de volta um valor que já existe
no `StateRegistry` (aqui, o próprio `#[state]` do atuador), então herda de graça o mesmo caminho
`Send + Sync` que os sensores de verdade já usam. Sem isso, o node ficava travado no valor inicial
pra sempre: só o write callback existia, nada nunca publicava a posição de volta.

`control: Arc<RuntimeControl>` (Send+Sync de verdade, só atomics por dentro — `runtime_control.rs`)
vira dois tipos de node, nenhum deles bridgeado por canal (diferente do write de atuador acima):
`clock.t_h` é só mais um node read-only na lista de push — `RuntimeControl` implementa `Sensor`
direto (`fn read(&self) -> f64 { self.t_h() }`), então entra em `push_nodes` como qualquer sensor de
verdade. `control.pause`/`control.resume`/`control.set_speed` viram `Method` nodes (`Call`, não
`Read`/`Write`) — cada callback chama o método correspondente de `RuntimeControl` diretamente, sem
canal: `RuntimeControl` não precisa da ponte que `Actuator` precisa porque já é `Send + Sync` por
inteiro, não só uma leitura-espelho dele. `reset` ainda não tem Method — ver nota em
`runtime_control.rs`.
*/

use std::collections::HashMap;
use std::sync::mpsc::Sender;
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

use crate::runtime_control::RuntimeControl;
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

/** Sobe um servidor OPC-UA: um node read-only por sensor em `sensors` (lido via `sensor.read()` a
cada tick — já passa pelo `SensorBehavior` do próprio sensor), um node read-write por nome em
`actuators` (lido via o `Sensor` espelho associado, escrito via `commands` — quem tem acesso de
verdade ao `Actuator` é a Thread da planta, nunca esta), mais um node read-only `clock.t_h` e três
`Method` (`control.pause`/`control.resume`/`control.set_speed`) a partir de `control` — ver nota no
topo do arquivo.

`endpoint` no formato `opc.tcp://<host>:<porta><path>`, ex.: `"opc.tcp://0.0.0.0:4840/tep/server/"`.

Bloqueia até o servidor encerrar (erro fatal — não há shutdown gracioso ainda).
*/
pub async fn serve(
    sensors: HashMap<String, Arc<dyn Sensor>>,
    actuators: HashMap<String, Arc<dyn Sensor>>,
    commands: Sender<(String, f64)>,
    control: Arc<RuntimeControl>,
    endpoint: &str,
) -> Result<(), String> {
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

    let push_nodes: Vec<(NodeId, Arc<dyn Sensor>)> = {
        let address_space = node_manager.address_space();
        let mut address_space = address_space.write();

        let folder_id = NodeId::new(ns, "signals");
        address_space.add_folder(
            &folder_id,
            "Signals",
            "Signals",
            &NodeId::objects_folder_id(),
        );

        let mut push_nodes: Vec<(NodeId, Arc<dyn Sensor>)> = Vec::new();

        for (name, sensor) in sensors {
            let node_id = NodeId::new(ns, name.clone());
            let _ = address_space.add_variables(
                vec![Variable::new(&node_id, name.as_str(), name.as_str(), 0f64)],
                &folder_id,
            );
            push_nodes.push((node_id, sensor));
        }

        /* Um único node por atuador, read-write — a mesma NodeId entra na lista de push (lida via
        o Sensor espelho) E ganha o write callback (comando entrando). Não são dois nodes: seria
        ambíguo pro cliente OPC-UA (qual dos dois é "a válvula X"?) e a UI nem deixaria escrever
        no que parece ser um node read-only.
        */
        for (name, shadow_sensor) in actuators {
            let node_id = NodeId::new(ns, name.clone());
            let mut var = Variable::new(&node_id, name.as_str(), name.as_str(), 0f64);
            /* `set_writable()` só mexe em `access_level` (capacidade do SERVIDOR) — o serviço de
            Write valida contra `user_access_level` (capacidade do USUÁRIO autenticado), que
            `Variable::new()` inicializa só com `CURRENT_READ`. Sem isso, todo Write cai em
            `BadUserAccessDenied` mesmo com `access_level` liberado.
            */
            var.set_writable(true);
            var.set_user_access_level(AccessLevel::CURRENT_READ | AccessLevel::CURRENT_WRITE);
            let _ = address_space.add_variables(vec![var], &folder_id);

            let commands = commands.clone();
            node_manager.inner().add_write_callback(
                node_id.clone(),
                move |data_value: DataValue, _range: &NumericRange| match data_value
                    .value
                    .as_ref()
                    .and_then(|v| v.as_f64())
                {
                    Some(value) => {
                        let _ = commands.send((name.clone(), value));
                        StatusCode::Good
                    }
                    None => StatusCode::BadTypeMismatch,
                },
            );

            push_nodes.push((node_id, shadow_sensor));
        }

        /* `RuntimeControl` implementa `Sensor` (runtime_control.rs) só pra expor `t_h` — entra na
        mesma lista de push que qualquer sensor de verdade, sem node manager/loop dedicado.
        */
        let clock_id = NodeId::new(ns, "clock.t_h");
        let _ = address_space.add_variables(
            vec![Variable::new(&clock_id, "clock.t_h", "clock.t_h", 0f64)],
            &folder_id,
        );
        push_nodes.push((clock_id, control.clone() as Arc<dyn Sensor>));

        /* Três Method (`Call`, não `Read`/`Write`) — cada callback chama `RuntimeControl`
        diretamente, sem canal: diferente da escrita de atuador acima, `RuntimeControl` já é
        `Send + Sync` por inteiro (só atomics por dentro), não uma leitura-espelho de algo `!Send`
        que precisa ser aplicado de volta na Thread da planta. `reset` fica de fora por enquanto —
        ver nota em `runtime_control.rs`.
        */
        let pause_id = NodeId::new(ns, "control.pause");
        MethodBuilder::new(&pause_id, "control.pause", "control.pause")
            .component_of(folder_id.clone())
            .insert(&mut *address_space);
        let control_for_pause = control.clone();
        node_manager
            .inner()
            .add_method_callback(pause_id, move |_args: &[Variant]| {
                control_for_pause.pause();
                Ok(Vec::new())
            });

        let resume_id = NodeId::new(ns, "control.resume");
        MethodBuilder::new(&resume_id, "control.resume", "control.resume")
            .component_of(folder_id.clone())
            .insert(&mut *address_space);
        let control_for_resume = control.clone();
        node_manager
            .inner()
            .add_method_callback(resume_id, move |_args: &[Variant]| {
                control_for_resume.resume();
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
        let control_for_speed = control.clone();
        node_manager
            .inner()
            .add_method_callback(speed_id, move |args: &[Variant]| {
                let factor = args
                    .first()
                    .and_then(Variant::as_f64)
                    .ok_or(StatusCode::BadInvalidArgument)?;
                control_for_speed.set_speed(factor);
                Ok(Vec::new())
            });

        push_nodes
    };

    let subscriptions = handle.subscriptions().clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;

            let updates: Vec<_> = push_nodes
                .iter()
                .map(|(node_id, sensor)| (node_id, None, DataValue::new_now(sensor.read())))
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
