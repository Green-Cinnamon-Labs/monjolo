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
    let ActuatorArgs { key, config } = match syn::parse::<ActuatorArgs>(attr) {
        Ok(args) => args,
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

    /* `config`: chave opcional do Snapshot pra semear o valor NOMINAL (command E state, os dois
    iguais — dynamics() nasce em zero, nada deriva sozinho até algo escrever um command diferente)
    — sem isso, todo atuador nascia em 0.0 incondicionalmente, mesmo quando `application.toml` já
    tinha o valor certo em `[state.valves]`. Sem `config`, comportamento inalterado (nasce em 0.0):
    nem todo atuador hipotético tem um "nominal" natural no Snapshot.
    */
    let initial_tokens = match &config {
        Some(config_key) => quote! { config.get(#config_key).unwrap_or(0.0) },
        None => quote! { 0.0 },
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
            pub fn new(
                registry: &mut ::monjolo::state_registry::StateRegistry,
                config: &::monjolo::snapshot::Snapshot,
            ) -> ::std::rc::Rc<Self> {
                let __derivative_key = ::std::format!("{}.derivative", #key);
                let (__offered, _) = registry.subscribe(&[#key, &__derivative_key], &[]);
                let __initial: f64 = #initial_tokens;
                __offered[0].set(__initial);
                let __instance = ::std::rc::Rc::new(Self {
                    #command_field: ::std::cell::Cell::new(__initial),
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
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, config: &::monjolo::snapshot::Snapshot| {
                    ::std::option::Option::Some(
                        ::std::boxed::Box::new(#struct_name::new(registry, config))
                            as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>,
                    )
                },
            }
        }
    };

    expanded.into()
}

struct ActuatorArgs {
    key: String,
    config: Option<String>,
}

impl syn::parse::Parse for ActuatorArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated(input)?;

        let mut key = None;
        let mut config = None;

        for pair in &pairs {
            if pair.path.is_ident("key") {
                key = Some(expect_str_lit(&pair.value)?);
            } else if pair.path.is_ident("config") {
                config = Some(expect_str_lit(&pair.value)?);
            } else {
                return Err(syn::Error::new_spanned(&pair.path, "esperado `key` ou `config`"));
            }
        }

        let key = key.ok_or_else(|| {
            syn::Error::new(proc_macro2::Span::call_site(), "falta `key = \"...\"`")
        })?;

        Ok(ActuatorArgs { key, config })
    }
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

/** `#[controller(name = "...")]` — mesmo padrão de `#[actuator(...)]`: a partir de uma struct com
campos `#[sensor(key = "...")]`/`#[actuator(key = "...")]` e um `impl` com um método `control(&self)`
escrito à mão em outro lugar, gera o wiring inteiro (`SensorHandle`/`ActuatorHandle`, `new()` que já
resolve os nomes via `need_sensor()`/`need_actuator()` — ordem-independente, Art. 6.3 — e já se
registra no catálogo via `offer_controller()`, `impl DynamicModel` chamando `control()`).

Cada campo marcado vira um getter que devolve o HANDLE (`Arc<dyn Sensor>`/`Rc<dyn Actuator>`), não o
valor já lido/escrito — mesma forma que `controller::model::Controller::sensor()`/`actuator()` já
expõem à mão. `control()` decide sozinho quando `.read()`/`.write(valor)`; a lei de controle (Kp,
setpoint, bias — o que for) fica inteiramente no método do usuário, nunca na macro — mesmo raciocínio
de `tau` dentro de `dynamics()` em `#[actuator]`.

Exige pelo menos um campo `#[sensor]` e um `#[actuator]` — um Controller sem nenhum dos dois não tem
com o que interagir. Múltiplos de cada são permitidos (malhas com mais de uma medição/atuação); um
campo não pode ser os dois ao mesmo tempo.
*/
#[proc_macro_attribute]
pub fn controller(attr: TokenStream, item: TokenStream) -> TokenStream {
    let name = match parse_name_arg(attr) {
        Ok(name) => name,
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
                "#[controller] só suporta struct com campos nomeados (ex.: `struct ReactorPressureControl { #[sensor(key = \"reactor.pressure\")] pressure: f64, #[actuator(key = \"valve.purge.position\")] purge: f64 }`)",
            )
            .to_compile_error()
            .into();
        }
    };

    let mut sensor_idents = Vec::new();
    let mut sensor_keys = Vec::new();
    let mut actuator_idents = Vec::new();
    let mut actuator_keys = Vec::new();

    for field in named_fields {
        let ident = field.ident.as_ref().expect("Fields::Named sempre tem ident");
        let mut marked = false;
        for field_attr in &field.attrs {
            if field_attr.path().is_ident("sensor") {
                if marked {
                    return syn::Error::new_spanned(field_attr, "um campo só pode ser #[sensor] OU #[actuator], não os dois")
                        .to_compile_error()
                        .into();
                }
                let key = match parse_field_key(field_attr) {
                    Ok(key) => key,
                    Err(err) => return err.to_compile_error().into(),
                };
                sensor_idents.push(ident.clone());
                sensor_keys.push(key);
                marked = true;
            } else if field_attr.path().is_ident("actuator") {
                if marked {
                    return syn::Error::new_spanned(field_attr, "um campo só pode ser #[sensor] OU #[actuator], não os dois")
                        .to_compile_error()
                        .into();
                }
                let key = match parse_field_key(field_attr) {
                    Ok(key) => key,
                    Err(err) => return err.to_compile_error().into(),
                };
                actuator_idents.push(ident.clone());
                actuator_keys.push(key);
                marked = true;
            }
        }
    }

    if sensor_idents.is_empty() {
        return syn::Error::new_spanned(&input, "falta pelo menos um campo marcado #[sensor(key = \"...\")]")
            .to_compile_error()
            .into();
    }
    if actuator_idents.is_empty() {
        return syn::Error::new_spanned(&input, "falta pelo menos um campo marcado #[actuator(key = \"...\")]")
            .to_compile_error()
            .into();
    }

    let expanded = quote! {
        #visibility struct #struct_name {
            #(#sensor_idents: ::monjolo::state_registry::SensorHandle,)*
            #(#actuator_idents: ::monjolo::state_registry::ActuatorHandle,)*
        }

        impl #struct_name {
            #(
                pub fn #sensor_idents(&self) -> ::std::sync::Arc<dyn ::monjolo::sensor::Sensor> {
                    self.#sensor_idents.sensor()
                }
            )*
            #(
                pub fn #actuator_idents(&self) -> ::std::rc::Rc<dyn ::monjolo::actuator::Actuator> {
                    self.#actuator_idents.actuator()
                }
            )*

            /** `new()` já registra o controller no catálogo do StateRegistry sob `name` — mesma
            invariante de `controller::model::Controller::new()` ("criado = já oferecido").
            */
            pub fn new(registry: &mut ::monjolo::state_registry::StateRegistry) -> ::std::rc::Rc<Self> {
                #(let #sensor_idents = registry.need_sensor(#sensor_keys);)*
                #(let #actuator_idents = registry.need_actuator(#actuator_keys);)*
                let __instance = ::std::rc::Rc::new(Self {
                    #(#sensor_idents,)*
                    #(#actuator_idents,)*
                });
                registry.offer_controller(#name, __instance.clone());
                __instance
            }
        }

        impl ::monjolo::dynamic_model::DynamicModel for #struct_name {
            fn evaluate(&self) {
                self.control();
            }
        }

        impl ::monjolo::controller::Controller for #struct_name {}

        /* Anúncio escondido pro bootstrap de Simulation — Controller É DynamicModel (fase C, ver
        monjolo::component), então construct() devolve Some: a mesma instância catalogada
        (offer_controller, dentro de new()) entra na árvore de avaliação.
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

fn parse_name_arg(attr: TokenStream) -> syn::Result<String> {
    let meta = syn::parse::<syn::MetaNameValue>(attr)?;
    if !meta.path.is_ident("name") {
        return Err(syn::Error::new_spanned(&meta.path, "esperado `name = \"...\"`"));
    }
    match &meta.value {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(literal), .. }) => Ok(literal.value()),
        other => Err(syn::Error::new_spanned(other, "`name` precisa ser uma string literal")),
    }
}

/* `#[sensor(key = "...")]`/`#[actuator(key = "...")]` — só esse único argumento, cada um (ao
contrário de `#[need]`/`#[offer]` em `#[dynamic_model]`, que também aceitam `prefix`+`components`
pra vetores: Sensor/Actuator são entradas escalares e nomeadas no catálogo, nunca um vetor).
*/
fn parse_field_key(attr: &syn::Attribute) -> syn::Result<String> {
    let meta = attr.parse_args::<syn::MetaNameValue>()?;
    if !meta.path.is_ident("key") {
        return Err(syn::Error::new_spanned(&meta.path, "esperado `key = \"...\"`"));
    }
    expect_str_lit(&meta.value)
}

fn expect_str_lit(expr: &syn::Expr) -> syn::Result<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(literal), .. }) => Ok(literal.value()),
        other => Err(syn::Error::new_spanned(other, "esperada uma string literal")),
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
