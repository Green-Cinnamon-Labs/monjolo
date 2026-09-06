/** monjolo-macros/tasks.rs

`#[monjolo::tasks]`, aplicado ao `impl X { ... }` INTEIRO de uma unidade `#[dynamic_model]` — não a
métodos individuais. Motivo: um atributo aplicado a um item DENTRO de um `impl` só pode ser
substituído por outros itens ASSOCIADOS (mais métodos) — a gramática de Rust não permite que a
expansão de um método solto produza `struct`/`impl`/`inventory::submit!` avulsos (só é válido onde
o item ORIGINAL já estava no nível de módulo). Aplicado ao `impl` inteiro, a macro já está nessa
posição e pode emitir, por método marcado, um struct-tarefa + `impl DynamicModel` +
`inventory::submit!` novos, ao lado do próprio `impl` (reescrito, com os métodos marcados
renomeados).

Cada método com `#[need(...)]`/`#[offer(...)]` empilhados vira uma tarefa: os PARÂMETROS (na ordem
declarada) são os `needs` — 1 `#[need(...)]` por parâmetro, mesma gramática `key = "..."` /
`prefix = "...", components = [...]` de `#[dynamic_model]` (reaproveitada de `dynamic_model.rs`,
não duplicada). O RETORNO é o(s) `offer(s)`: um `#[offer(...)]` só = tipo de retorno direto; 2+ =
tupla, na mesma ordem dos atributos. Métodos SEM `#[need]`/`#[offer]` ficam intocados, comuns, fora
do scheduler — únicos que podem ler `self.campo()` livremente por dentro do `impl` reescrito.

Cada tarefa gerada guarda `Rc<X>` (a instância compartilhada da unidade, buscada via
`registry.instance::<X>(...)` — ver `state_registry.rs`) + um `Proxy`/`[Proxy; N]` por need/offer,
resolvidos UMA vez em `construct()` (bootstrap), nunca por tick. `after` é auto-injetado com o nome
da própria unidade (`stringify!(X)`) — garante que ela já foi construída (e já chamou
`offer_instance`) antes de qualquer tarefa seguir em frente; ver `component::sort_phase_a`.
*/
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{ImplItem, ImplItemFn, ItemImpl, Type};

use crate::dynamic_model::{field_init_from_slice, parse_key_spec, FieldKeySpec};

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[monjolo::tasks] não aceita argumentos",
        )
        .to_compile_error()
        .into();
    }

    let input = syn::parse_macro_input!(item as ItemImpl);

    if input.trait_.is_some() {
        return syn::Error::new_spanned(&input, "#[monjolo::tasks] só suporta impl inerente (sem trait)")
            .to_compile_error()
            .into();
    }

    let self_ty = input.self_ty.as_ref();
    let owner_ident = match self_ty_ident(self_ty) {
        Ok(ident) => ident,
        Err(err) => return err.to_compile_error().into(),
    };
    let impl_attrs = &input.attrs;
    let generics = &input.generics;

    let mut kept_items: Vec<TokenStream2> = Vec::new();
    let mut task_defs: Vec<TokenStream2> = Vec::new();

    for item in input.items {
        match item {
            ImplItem::Fn(method) => {
                let has_marker = method
                    .attrs
                    .iter()
                    .any(|a| a.path().is_ident("need") || a.path().is_ident("offer"));

                if !has_marker {
                    kept_items.push(quote! { #method });
                    continue;
                }

                match build_task(self_ty, owner_ident, &method) {
                    Ok((impl_method, task_def)) => {
                        kept_items.push(impl_method);
                        task_defs.push(task_def);
                    }
                    Err(err) => return err.to_compile_error().into(),
                }
            }
            other => kept_items.push(quote! { #other }),
        }
    }

    let expanded = quote! {
        #(#impl_attrs)*
        impl #generics #self_ty {
            #(#kept_items)*
        }

        #(#task_defs)*
    };

    expanded.into()
}

fn self_ty_ident(self_ty: &Type) -> syn::Result<&syn::Ident> {
    match self_ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|segment| &segment.ident)
            .ok_or_else(|| {
                syn::Error::new_spanned(self_ty, "#[monjolo::tasks] precisa de um tipo simples (ex.: `impl Reactor`)")
            }),
        _ => Err(syn::Error::new_spanned(
            self_ty,
            "#[monjolo::tasks] precisa de um tipo simples (ex.: `impl Reactor`)",
        )),
    }
}

/* Constrói UMA tarefa a partir de um método marcado: devolve (a) o método reescrito — mesmo corpo,
renomeado `__{nome}_impl`, `#[need]`/`#[offer]` removidos — pra ficar dentro do `impl X` reescrito;
(b) a definição completa da tarefa (struct + `impl DynamicModel` + `inventory::submit!`), pra ficar
ao lado, no nível de módulo.
*/
fn build_task(
    self_ty: &Type,
    owner_ident: &syn::Ident,
    method: &ImplItemFn,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let method_name = &method.sig.ident;
    let impl_name = format_ident!("__{}_impl", method_name);

    let mut need_specs: Vec<FieldKeySpec> = Vec::new();
    let mut offer_specs: Vec<FieldKeySpec> = Vec::new();
    /* Atributos que não são `#[need]`/`#[offer]` (doc comments, `#[allow(...)]`, etc.) não são
    erro — `impl_method.attrs.retain(...)` abaixo já os preserva no método renomeado, intocados.
    Só `need`/`offer` são consumidos aqui.
    */
    for attr in &method.attrs {
        if attr.path().is_ident("need") {
            need_specs.push(parse_key_spec(attr)?);
        } else if attr.path().is_ident("offer") {
            offer_specs.push(parse_key_spec(attr)?);
        }
    }

    if offer_specs.is_empty() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "método marcado com #[need]/#[offer] precisa de pelo menos um #[offer(...)] — sem \
            offer nenhum, esta tarefa não publicaria nada",
        ));
    }

    let params: Vec<&syn::PatType> = method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(pat_ty) => Some(pat_ty),
            syn::FnArg::Receiver(_) => None,
        })
        .collect();

    if params.len() != need_specs.len() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            format!(
                "{} parâmetro(s) declarado(s) mas {} atributo(s) #[need(...)] — precisa bater \
                1:1, na ordem em que aparecem (1º #[need] = 1º parâmetro, etc.)",
                params.len(),
                need_specs.len(),
            ),
        ));
    }

    let mut need_keys: Vec<String> = Vec::new();
    let mut need_proxy_fields: Vec<TokenStream2> = Vec::new();
    let mut need_field_inits: Vec<TokenStream2> = Vec::new();
    let mut need_value_exprs: Vec<TokenStream2> = Vec::new();

    for spec in &need_specs {
        let field_ident = format_ident!("__need_{}", need_value_exprs.len());
        let start = need_keys.len();
        let keys = spec.keys();
        let len = keys.len();
        need_keys.extend(keys);

        let n = match spec {
            FieldKeySpec::Scalar(_) => None,
            FieldKeySpec::Array(_, components) => Some(components.len()),
        };

        need_field_inits.push(field_init_from_slice(&field_ident, "__needed", start, len, n));

        match n {
            None => {
                need_proxy_fields.push(quote! { #field_ident: ::monjolo::state_registry::Proxy });
                need_value_exprs.push(quote! { self.#field_ident.get() });
            }
            Some(count) => {
                need_proxy_fields.push(quote! { #field_ident: [::monjolo::state_registry::Proxy; #count] });
                let indices = (0..count).map(syn::Index::from);
                need_value_exprs.push(quote! { [#(self.#field_ident[#indices].get()),*] });
            }
        }
    }

    let multiple_offers = offer_specs.len() > 1;

    let mut offer_keys: Vec<String> = Vec::new();
    let mut offer_proxy_fields: Vec<TokenStream2> = Vec::new();
    let mut offer_field_inits: Vec<TokenStream2> = Vec::new();
    let mut offer_set_stmts: Vec<TokenStream2> = Vec::new();
    let mut result_bindings: Vec<syn::Ident> = Vec::new();

    for spec in &offer_specs {
        let index = offer_set_stmts.len();
        let field_ident = format_ident!("__offer_{}", index);
        let start = offer_keys.len();
        let keys = spec.keys();
        let len = keys.len();
        offer_keys.extend(keys);

        let n = match spec {
            FieldKeySpec::Scalar(_) => None,
            FieldKeySpec::Array(_, components) => Some(components.len()),
        };

        offer_field_inits.push(field_init_from_slice(&field_ident, "__offered", start, len, n));

        let result_binding = if multiple_offers {
            format_ident!("__result_{}", index)
        } else {
            format_ident!("__result")
        };

        match n {
            None => {
                offer_proxy_fields.push(quote! { #field_ident: ::monjolo::state_registry::Proxy });
                offer_set_stmts.push(quote! { self.#field_ident.set(#result_binding); });
            }
            Some(count) => {
                offer_proxy_fields.push(quote! { #field_ident: [::monjolo::state_registry::Proxy; #count] });
                let indices = (0..count).map(syn::Index::from);
                offer_set_stmts.push(quote! {
                    #(self.#field_ident[#indices].set(#result_binding[#indices]);)*
                });
            }
        }
        result_bindings.push(result_binding);
    }

    let call_and_distribute = if multiple_offers {
        quote! {
            let (#(#result_bindings),*) = self.__owner.#impl_name(#(#need_value_exprs),*);
            #(#offer_set_stmts)*
        }
    } else {
        quote! {
            let __result = self.__owner.#impl_name(#(#need_value_exprs),*);
            #(#offer_set_stmts)*
        }
    };

    // Método reescrito: mesmo corpo/assinatura, renomeado, sem os atributos #[need]/#[offer] (não
    // são atributos de verdade — sobreviver até o compilador seria "cannot find attribute").
    let mut impl_method = method.clone();
    impl_method
        .attrs
        .retain(|a| !(a.path().is_ident("need") || a.path().is_ident("offer")));
    impl_method.sig.ident = impl_name.clone();
    let impl_method_tokens = quote! { #impl_method };

    let task_struct_name = format_ident!("__{}_{}_Task", owner_ident, method_name);
    let descriptor_name: String = format!("{}::{}", owner_ident, method_name);
    let offer_refs = quote! { &[#(#offer_keys),*] };
    let need_refs = quote! { &[#(#need_keys),*] };

    let task_def = quote! {
        #[allow(non_camel_case_types)]
        struct #task_struct_name {
            __owner: ::std::rc::Rc<#self_ty>,
            #(#need_proxy_fields,)*
            #(#offer_proxy_fields,)*
        }

        impl ::monjolo::dynamic_model::DynamicModel for #task_struct_name {
            fn name(&self) -> &str {
                #descriptor_name
            }

            fn evaluate(&self) {
                #call_and_distribute
            }
        }

        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: #descriptor_name,
                kind: ::monjolo::ComponentKind::Dynamic,
                after: &[::std::stringify!(#self_ty)],
                needs: #need_refs,
                offers: #offer_refs,
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, _config: &::monjolo::snapshot::Snapshot| {
                    let __owner = registry.instance::<#self_ty>(::std::stringify!(#self_ty)).unwrap_or_else(|| {
                        ::std::panic!(
                            "'{}' deveria já ter sido construída (offer_instance) antes de suas \
                            tarefas — `after` deveria garantir isso",
                            ::std::stringify!(#self_ty),
                        )
                    });
                    let (__offered, __needed) = registry.subscribe(#offer_refs, #need_refs);
                    ::std::option::Option::Some(::std::boxed::Box::new(#task_struct_name {
                        __owner,
                        #(#need_field_inits,)*
                        #(#offer_field_inits,)*
                    }) as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>)
                },
            }
        }
    };

    Ok((impl_method_tokens, task_def))
}
