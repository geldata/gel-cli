use proc_macro2::TokenStream;
use quote::quote;

use crate::types;

pub(crate) fn mk_setting_impl(e: &types::Enum) -> TokenStream {
    let ident = &e.ident;
    let (impl_gen, ty_gen, where_cl) = e.generics.split_for_impl();
    let to_string = e.subcommands.iter().map(|sub| {
        let variant = &sub.ident;
        let name = ::heck::ToKebabCase::to_kebab_case(&variant.to_string()[..]);
        quote! {
            #ident::#variant(..) => #name
        }
    });
    let is_show = e.subcommands.iter().map(|sub| {
        let variant = &sub.ident;
        quote! {
            #ident::#variant(val) => val.value.is_none()
        }
    });
    let all_items = e.subcommands.iter().map(|sub| {
        let variant = &sub.ident;
        quote! {
            #ident::#variant(::std::default::Default::default())
        }
    });
    quote! {
        impl #impl_gen #ident #ty_gen #where_cl
        {
            pub fn name(&self) -> &'static str {
                match self {
                    #( #to_string ),*
                }
            }
            pub fn is_show(&self) -> bool {
                use Setting::*;

                match self {
                    #( #is_show ),*
                }
            }
            pub fn all_items() -> &'static [#ident] {
                static SETTINGS: ::once_cell::sync::OnceCell<Vec<#ident>>
                    = ::once_cell::sync::OnceCell::new();
                return &SETTINGS.get_or_init(|| {
                    vec![#( #all_items ),*]
                })[..];
            }
        }
    }
}
