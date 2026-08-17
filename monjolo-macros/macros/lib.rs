/* monjolo-macros/lib.rs — RASCUNHO (branch feat/proc-macro-components).

`actuator`: gera, a partir de uma struct com campos `#[command]`/`#[state]` e um `impl` com um
método `dynamics(&self) -> f64` escrito à mão em outro lugar, exatamente o padrão que
`actuator::model::Actuator` já implementa hoje à mão: `command: Cell<f64>`, `state`/`derivative`
como `Proxy`, `write()`/`evaluate()`, `new()` que já se registra no catálogo sob a própria chave.

Decisão (conversada antes de escrever isto): sem reescrita de expressão nenhuma dentro do `dynamics()`
do usuário — os campos viram getters (`self.command()`, não `self.command`), gerados do lado da
struct. O `impl` com `dynamics()` que o usuário escreve fica inteiramente intocado pela macro; só
precisa existir, em algum lugar, um método com esse nome e essa assinatura — se não existir, o erro
aparece no `self.dynamics()` gerado, um erro comum do compilador, não de macro.

`sensor`: mais simples — `Sensor` (sensor/model.rs) não tem computação própria do usuário, só
key + `SensorBehavior` (Ideal/Noisy/Hysteresis, um enum fechado, não uma closure arbitrária). Por
isso a struct anotada não precisa de campos: é só um nome pra pendurar `new()`/o `inventory::submit!`
— sempre `Ideal` por enquanto (seleção de behavior via atributo fica pra quando isso entrar em
escopo, ver docs/06-ruidos.md).

`dynamic_model`: o mais geral dos quatro — ver dynamic_model.rs (implementação grande o bastante
pra merecer arquivo próprio; `#[proc_macro_attribute]` precisa ficar na raiz do crate por exigência
do rustc, então esta função aqui só delega pro módulo).

Todas emitem um `inventory::submit!` escondido (ver `monjolo::component`) — a própria declaração
anotada já é suficiente pro componente se anunciar ao bootstrap de `Simulation`, sem chamada manual
a `add_dynamic`/`offer_*` em lugar nenhum.
*/

mod dynamic_model;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Fields, ItemStruct};

#[proc_macro_attribute]
pub fn dynamic_model(attr: TokenStream, item: TokenStream) -> TokenStream {
    dynamic_model::expand(attr, item)
}

#[proc_macro_attribute]
pub fn actuator(attr: TokenStream, item: TokenStream) -> TokenStream {
    let key = match parse_key_arg(attr) {
        Ok(key) => key,
        Err(err) => return err.to_compile_error().into(),
    };

    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;
    let visibility = &input.vis;

    let named_fields = match &input.fields {
        Fields::Named(named) => &named.named,
        _ => {
            return syn::Error::new_spanned(
                &input,
                "#[actuator] só suporta struct com campos nomeados (ex.: `struct FeedD { #[command] command: f64, #[state] position: f64 }`)",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut command_field = None;
    let mut state_field = None;

    for field in named_fields {
        let ident = field.ident.as_ref().expect("Fields::Named sempre tem ident");
        for field_attr in &field.attrs {
            if field_attr.path().is_ident("command") {
                if command_field.is_some() {
                    return syn::Error::new_spanned(field_attr, "só pode haver um campo #[command]")
                        .to_compile_error()
                        .into();
                }
                command_field = Some(ident.clone());
            } else if field_attr.path().is_ident("state") {
                if state_field.is_some() {
                    return syn::Error::new_spanned(field_attr, "só pode haver um campo #[state]")
                        .to_compile_error()
                        .into();
                }
                state_field = Some(ident.clone());
            }
        }
    }

    let command_field = match command_field {
        Some(field) => field,
        None => {
            return syn::Error::new_spanned(&input, "falta um campo marcado #[command]")
                .to_compile_error()
                .into()
        }
    };
    let state_field = match state_field {
        Some(field) => field,
        None => {
            return syn::Error::new_spanned(&input, "falta um campo marcado #[state]")
                .to_compile_error()
                .into()
        }
    };

    /* O nome do campo vira também o nome do getter gerado — Rust permite campo e método com o
    mesmo nome na mesma struct (namespaces diferentes, desambiguados por `()`); é assim que
    `self.command()` (getter) convive com o campo interno `command` (agora um `Cell<f64>` real,
    não mais o `f64` que o usuário declarou).
    */
    let expanded = quote! {
        #visibility struct #struct_name {
            #command_field: ::std::cell::Cell<f64>,
            #state_field: ::monjolo::state_registry::Proxy,
            __derivative: ::monjolo::state_registry::Proxy,
        }

        impl #struct_name {
            pub fn #command_field(&self) -> f64 {
                self.#command_field.get()
            }

            pub fn #state_field(&self) -> f64 {
                self.#state_field.get()
            }

            /** `new()` já registra o atuador no catálogo do StateRegistry sob a própria chave —
            mesma invariante de `actuator::model::Actuator::new()`: "criado = já oferecido".
            */
            pub fn new(registry: &mut ::monjolo::state_registry::StateRegistry) -> ::std::rc::Rc<Self> {
                let __derivative_key = ::std::format!("{}.derivative", #key);
                let (__offered, _) = registry.subscribe(&[#key, &__derivative_key], &[]);
                let __instance = ::std::rc::Rc::new(Self {
                    #command_field: ::std::cell::Cell::new(0.0),
                    #state_field: __offered[0].clone(),
                    __derivative: __offered[1].clone(),
                });
                registry.offer_actuator(#key, __instance.clone());
                __instance
            }
        }

        impl ::monjolo::actuator::Actuator for #struct_name {
            fn write(&self, value: f64) {
                self.#command_field.set(value);
            }
        }

        impl ::monjolo::dynamic_model::DynamicModel for #struct_name {
            fn evaluate(&self) {
                let __derivative = self.dynamics();
                self.__derivative.set(__derivative);
            }

            /* Invariante do framework (dynamic_model.rs::Composite::state_keys()): todo
            DynamicModel que declara estado integrável precisa que o Integrator enxergue isso —
            #state_field é a mesma chave que Actuator::model::Actuator declararia à mão.
            */
            fn state_keys(&self) -> ::std::vec::Vec<::std::string::String> {
                ::std::vec![#key.to_string()]
            }
        }

        /* Anúncio escondido pro bootstrap de Simulation (ver monjolo::component) — nem
        build_tep() nem main() precisam saber que #struct_name existe: a própria declaração
        anotada já é suficiente. Roda exatamente uma vez, no bootstrap; a instância que resulta
        vive pelo resto da simulação.
        */
        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: ::std::stringify!(#struct_name),
                kind: ::monjolo::ComponentKind::Actuator,
                after: &[],
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, _config: &::monjolo::snapshot::Snapshot| {
                    ::std::option::Option::Some(
                        ::std::boxed::Box::new(#struct_name::new(registry))
                            as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>,
                    )
                },
            }
        }
    };

    expanded.into()
}

#[proc_macro_attribute]
pub fn sensor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let key = match parse_key_arg(attr) {
        Ok(key) => key,
        Err(err) => return err.to_compile_error().into(),
    };

    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;
    let visibility = &input.vis;

    if !matches!(input.fields, Fields::Unit) {
        return syn::Error::new_spanned(
            &input,
            "#[sensor] só suporta struct sem campos (ex.: `struct ReactorTemperature;`) — a leitura não tem computação própria do usuário, só key + behavior (Ideal por enquanto)",
        )
        .to_compile_error()
        .into();
    }

    let expanded = quote! {
        #visibility struct #struct_name;

        impl #struct_name {
            /** Devolve o mesmo `Arc<Sensor>` que o catálogo do StateRegistry guarda — "criado = já
            oferecido", mesma invariante de `sensor::model::Sensor::new()` (que este `new()` só
            encapsula: key fixa, sempre `Ideal` por enquanto).
            */
            pub fn new(
                registry: &mut ::monjolo::state_registry::StateRegistry,
            ) -> ::std::sync::Arc<::monjolo::sensor::model::Sensor> {
                ::monjolo::sensor::model::Sensor::new(
                    registry,
                    #key,
                    ::std::boxed::Box::new(::monjolo::sensor::model::Ideal),
                )
            }
        }

        /* Anúncio escondido pro bootstrap de Simulation — Sensor nunca é DynamicModel (leitura sob
        demanda, não avaliada por tick), então construct() sempre devolve None: chamar new() já
        cataloga via offer_sensor(), nada mais precisa acontecer aqui.
        */
        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: ::std::stringify!(#struct_name),
                kind: ::monjolo::ComponentKind::Sensor,
                after: &[],
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, _config: &::monjolo::snapshot::Snapshot| {
                    #struct_name::new(registry);
                    ::std::option::Option::None
                },
            }
        }
    };

    expanded.into()
}

/** `#[controller(name = "...", sensors = ["..."], actuators = ["..."])]` — embrulha
`controller::model::Controller::new()`, que já resolve os nomes de Sensor/Actuator declarados via
`need_sensor()`/`need_actuator()` (ordem-independente, Art. 6.3). Não inventa lógica de controle
nenhuma — `impl DynamicModel for Controller` (com `evaluate()` vazio) mora em
`monjolo::controller::model`, não aqui: um único `impl`, escrito à mão, cobre todo Controller,
macro ou não (ver o comentário lá pra explicar por quê não pode ser gerado por invocação daqui).
`sensors`/`actuators` são opcionais — default `[]`, mesmo default de `Controller::new()`.
*/
#[proc_macro_attribute]
pub fn controller(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match syn::parse::<ControllerArgs>(attr) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };

    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;
    let visibility = &input.vis;

    if !matches!(input.fields, Fields::Unit) {
        return syn::Error::new_spanned(
            &input,
            "#[controller] só suporta struct sem campos (ex.: `struct ReactorPressureControl;`) — ainda não há lógica de controle própria do usuário, só nomes de Sensor/Actuator (Controller é open item, ver CONTRIBUTING.md)",
        )
        .to_compile_error()
        .into();
    }

    let name = &args.name;
    let sensors = &args.sensors;
    let actuators = &args.actuators;

    let expanded = quote! {
        #visibility struct #struct_name;

        impl #struct_name {
            /** Devolve o mesmo `Rc<Controller>` que o catálogo do StateRegistry guarda — mesma
            invariante de `controller::model::Controller::new()` (que este `new()` só encapsula).
            */
            pub fn new(
                registry: &mut ::monjolo::state_registry::StateRegistry,
            ) -> ::std::rc::Rc<::monjolo::controller::model::Controller> {
                ::monjolo::controller::model::Controller::new(
                    registry,
                    #name,
                    &[#(#sensors),*],
                    &[#(#actuators),*],
                )
            }
        }

        /* Anúncio escondido pro bootstrap de Simulation — Controller É DynamicModel (fase C, ver
        monjolo::component), então construct() devolve Some: a mesma instância catalogada
        (offer_controller, dentro de Controller::new()) entra na árvore de avaliação.
        */
        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: ::std::stringify!(#struct_name),
                kind: ::monjolo::ComponentKind::Controller,
                after: &[],
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, _config: &::monjolo::snapshot::Snapshot| {
                    ::std::option::Option::Some(
                        ::std::boxed::Box::new(#struct_name::new(registry))
                            as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>,
                    )
                },
            }
        }
    };

    expanded.into()
}

struct ControllerArgs {
    name: String,
    sensors: Vec<String>,
    actuators: Vec<String>,
}

impl syn::parse::Parse for ControllerArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated(input)?;

        let mut name = None;
        let mut sensors = Vec::new();
        let mut actuators = Vec::new();

        for pair in &pairs {
            if pair.path.is_ident("name") {
                name = Some(expect_str_lit(&pair.value)?);
            } else if pair.path.is_ident("sensors") {
                sensors = expect_str_array(&pair.value)?;
            } else if pair.path.is_ident("actuators") {
                actuators = expect_str_array(&pair.value)?;
            } else {
                return Err(syn::Error::new_spanned(
                    &pair.path,
                    "esperado `name`, `sensors` ou `actuators`",
                ));
            }
        }

        let name = name.ok_or_else(|| {
            syn::Error::new(proc_macro2::Span::call_site(), "falta `name = \"...\"`")
        })?;

        Ok(ControllerArgs { name, sensors, actuators })
    }
}

fn expect_str_lit(expr: &syn::Expr) -> syn::Result<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(literal), .. }) => Ok(literal.value()),
        other => Err(syn::Error::new_spanned(other, "esperada uma string literal")),
    }
}

fn expect_str_array(expr: &syn::Expr) -> syn::Result<Vec<String>> {
    match expr {
        syn::Expr::Array(array) => array.elems.iter().map(expect_str_lit).collect(),
        other => Err(syn::Error::new_spanned(other, "esperado um array de strings, ex.: [\"a\", \"b\"]")),
    }
}

/* Parseia `key = "valve.feed_d.position"` — o único argumento que o atributo aceita por enquanto. */
fn parse_key_arg(attr: TokenStream) -> syn::Result<String> {
    let meta = syn::parse::<syn::MetaNameValue>(attr)?;
    if !meta.path.is_ident("key") {
        return Err(syn::Error::new_spanned(&meta.path, "esperado `key = \"...\"`"));
    }
    match &meta.value {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(literal), .. }) => Ok(literal.value()),
        other => Err(syn::Error::new_spanned(other, "`key` precisa ser uma string literal")),
    }
}
