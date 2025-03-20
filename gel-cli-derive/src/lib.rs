extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro_error2::abort;
use quote::quote;
use syn::parse_macro_input;

mod attrib;
mod into_app;
mod into_args;
mod kw;
mod types;

#[proc_macro_error2::proc_macro_error]
#[proc_macro_derive(EdbSettings)]
pub fn edgedb_edb_settings(input: TokenStream) -> TokenStream {
    let inp = parse_macro_input!(input as syn::Item);
    derive_edb_settings(inp).into()
}

fn derive_edb_settings(item: syn::Item) -> proc_macro2::TokenStream {
    let attrs = match item {
        syn::Item::Enum(ref e) => &e.attrs,
        _ => abort!(item, "can only derive EdbSettings for enums"),
    };
    let attrs = attrib::ContainerAttrs::from_syn(attrs);
    match item {
        syn::Item::Enum(e) => {
            let mut subcommands = Vec::new();
            for sub in e.variants {
                let ty = match sub.fields {
                    syn::Fields::Unit => None,
                    syn::Fields::Named(_) => {
                        abort!(sub, "named fields are not supported for EdbClap");
                    }
                    syn::Fields::Unnamed(mut unn) => {
                        if unn.unnamed.len() != 1 {
                            abort!(unn, "single field required");
                        }
                        Some(unn.unnamed.pop().unwrap().into_value().ty)
                    }
                };
                subcommands.push(types::Subcommand {
                    attrs: attrib::SubcommandAttrs::from_syn(&sub.attrs),
                    ident: sub.ident,
                    ty,
                });
            }

            let e = &types::Enum {
                attrs,
                vis: e.vis,
                ident: e.ident,
                generics: e.generics,
                subcommands,
            };

            let setting = into_app::mk_setting_impl(e);
            quote! {
                #setting
            }
        }
        _ => abort!(item, "can only derive EdbClap for structs and enums"),
    }
}

#[proc_macro_error2::proc_macro_error]
#[proc_macro_derive(IntoArgs, attributes(arg, command))]
pub fn edgedb_into_args(input: TokenStream) -> TokenStream {
    let inp = parse_macro_input!(input as syn::Item);
    derive_args(inp).into()
}

fn derive_args(item: syn::Item) -> proc_macro2::TokenStream {
    let attrs = match item {
        syn::Item::Struct(ref s) => &s.attrs,
        syn::Item::Enum(ref e) => &e.attrs,
        _ => abort!(item, "can only derive EdbClap for structs and enums"),
    };
    let attrs = attrib::ContainerAttrs::from_syn(attrs);
    match item {
        syn::Item::Struct(s) => {
            let fields = match s.fields {
                syn::Fields::Named(f) => f
                    .named
                    .into_iter()
                    .map(|f| types::Field::new(attrib::FieldAttrs::from_syn(&f.attrs), f))
                    .collect::<Vec<_>>(),
                _ => abort!(s, "only named fields are supported for EdbClap"),
            };
            into_args::structure(&types::Struct {
                attrs,
                vis: s.vis,
                ident: s.ident,
                generics: s.generics,
                fields,
            })
        }
        _ => abort!(item, "can only derive EdbClap for structs"),
    }
}
