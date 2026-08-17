/* examples/opcua_browse.rs

Cliente de fumaça pro adaptador OPC-UA (`adapter/opcua.rs`) — prova ponta a ponta, sem depender de
instalar uma ferramenta GUI externa (UaExpert etc.), que o servidor de verdade expõe os Sensors/
Actuators catalogados em StateRegistry: conecta, faz browse da pasta "Signals" (criada por
`opcua::serve()` sob o Objects folder padrão), lê o valor atual de cada node encontrado, e
opcionalmente escreve num node de atuador — provando a ponte por canal `(nome, valor)` até a Thread
da planta (`Simulation::spawn_plant_thread`, que loga cada escrita aplicada).

Requer `tep-plant` (ou outro binário que chame `Simulation::set_adapter`) já rodando, escutando em
`opc.tcp://127.0.0.1:4840/tep/server/` (endpoint default de `tep-plant/src/main.rs`).

Uso:
  cargo run --example opcua_browse --features opcua
  cargo run --example opcua_browse --features opcua -- write valve.feed_d.position 55.0
*/

use std::env;

use opcua::client::{ClientBuilder, IdentityToken};
use opcua::types::{
    BrowseDescription, BrowseDirection, BrowseResultMask, EndpointDescription,
    MessageSecurityMode, NodeId, ReadValueId, ReferenceTypeId, TimestampsToReturn,
    UserTokenPolicy, Variant, WriteValue,
};

const ENDPOINT_URL: &str = "opc.tcp://127.0.0.1:4840/tep/server/";

/* Mesma forma nos dois browses (Objects → "Signals", "Signals" → nodes) — só muda `node_id`. */
fn browse_children(node_id: NodeId) -> BrowseDescription {
    BrowseDescription {
        node_id,
        browse_direction: BrowseDirection::Forward,
        reference_type_id: ReferenceTypeId::HierarchicalReferences.into(),
        include_subtypes: true,
        node_class_mask: 0,
        result_mask: BrowseResultMask::All as u32,
    }
}

#[tokio::main]
async fn main() {
    let mut client = ClientBuilder::new()
        .application_name("monjolo opcua_browse smoke client")
        .application_uri("urn:monjolo:opcua-browse")
        .create_sample_keypair(true)
        .trust_server_certs(true)
        .session_retry_limit(3)
        .client()
        .expect("falha ao construir o ClientBuilder");

    let endpoint: EndpointDescription = (
        ENDPOINT_URL,
        "None",
        MessageSecurityMode::None,
        UserTokenPolicy::anonymous(),
    )
        .into();

    let (session, event_loop) = client
        .connect_to_matching_endpoint(endpoint, IdentityToken::Anonymous)
        .await
        .expect("falha ao conectar no servidor OPC-UA — tep-plant está rodando com --features opcua?");
    let _handle = event_loop.spawn();
    session.wait_for_connection().await;

    println!("[opcua_browse] conectado em {ENDPOINT_URL}");

    let objects_children = session
        .browse(&[browse_children(NodeId::objects_folder_id())], 0, None)
        .await
        .expect("browse do Objects folder falhou");

    let signals_ref = objects_children
        .into_iter()
        .flat_map(|result| result.references.unwrap_or_default())
        .find(|reference| reference.display_name.to_string() == "Signals")
        .expect("pasta \"Signals\" não encontrada — o servidor subiu sem nenhum sensor/atuador?");

    let signals_folder_id = signals_ref.node_id.node_id;
    println!("[opcua_browse] pasta \"Signals\" encontrada: {signals_folder_id}");

    let signal_results = session
        .browse(&[browse_children(signals_folder_id)], 0, None)
        .await
        .expect("browse de Signals falhou");

    let signal_refs: Vec<_> = signal_results
        .into_iter()
        .flat_map(|result| result.references.unwrap_or_default())
        .collect();

    println!("[opcua_browse] {} node(s) em Signals:", signal_refs.len());

    let read_ids: Vec<ReadValueId> = signal_refs
        .iter()
        .map(|reference| reference.node_id.node_id.clone().into())
        .collect();
    let values = session
        .read(&read_ids, TimestampsToReturn::Neither, 0.0)
        .await
        .expect("read falhou");

    for (reference, value) in signal_refs.iter().zip(values.iter()) {
        let raw = value.value.as_ref().and_then(Variant::as_f64);
        println!(
            "  - {} ({}) = {raw:?}",
            reference.display_name, reference.node_id.node_id,
        );
    }

    /* Escrita opcional — prova a ponte por canal até a Thread da planta (`spawn_plant_thread`
    drena `command_rx` e chama `actuator.write()`; ver log do processo tep-plant pra confirmar).
    */
    let mut args = env::args().skip(1);
    if args.next().as_deref() == Some("write") {
        let node_name = args.next().expect("uso: ... write <nome-do-node> <valor>");
        let value: f64 = args
            .next()
            .expect("uso: ... write <nome-do-node> <valor>")
            .parse()
            .expect("valor inválido — esperava um f64");

        let target = signal_refs
            .iter()
            .find(|reference| reference.display_name.to_string() == node_name)
            .unwrap_or_else(|| panic!("node \"{node_name}\" não encontrado em Signals"));

        let status = session
            .write(&[WriteValue::value_attr(
                target.node_id.node_id.clone(),
                Variant::from(value),
            )])
            .await
            .expect("write falhou");
        println!("[opcua_browse] escrita em {node_name} = {value}: {status:?}");
    }
}
