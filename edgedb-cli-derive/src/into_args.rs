use proc_macro_error::abort;
use proc_macro2::TokenStream;
use quote::quote;

use crate::attrib::ParserKind;
use crate::types;

pub fn structure(s: &types::Struct) -> TokenStream {
    use ParserKind::*;

    let ident = &s.ident;
    let (impl_gen, ty_gen, where_cl) = s.generics.split_for_impl();

    let mut args = Vec::new();
    for field in &s.fields {
        let ident = &field.ident;

        let mut long = field.attrs.long.as_ref().map(|long| {
            long.as_ref()
                .map(|s| s.value().to_string())
                .unwrap_or_else(|| s.attrs.rename_all.convert(&field.ident.to_string()))
        });
        if field.attrs.from_global {
            long = Some(s.attrs.rename_all.convert(&field.ident.to_string()));
        }

        if field.attrs.flatten {
            if field.optional {
                args.push(quote! {
                    if let Some(value) = &self.#ident {
                        process.args(value);
                    }
                });
            } else {
                args.push(quote! {
                    process.args(&self.#ident);
                });
            }
        } else if field.attrs.subcommand {
            abort!(field.ident, "subcommand is not implemented");
        } else if let Some(long) = long {
            let long = format!("--{long}");
            if field.multiple {
                abort!(field.ident, "multiple is not implemented");
            }
            match field.parse.kind {
                FromOccurrences => {
                    abort!(field.ident, "occurrences are not implemented");
                }
                FromStr | FromOsStr | TryFromStr | TryFromOsStr => {
                    if field.optional {
                        args.push(quote! {
                            if let Some(value) = &self.#ident {
                                process.arg(#long).args([value]);
                            }
                        });
                    // TODO(tailhook) maybe done, but the problem is that
                    //   default_value is a string
                    //} else if let Some(val) = &field.attrs.default_value {
                    //    args.push(quote! {
                    //        if self.#ident != #val {
                    //            process.arg(#long).arg(&self.#ident);
                    //        }
                    //    });
                    } else {
                        args.push(quote! {
                            process.arg(#long).args([&self.#ident]);
                        });
                    }
                }
                ValueEnum => {
                    if field.optional {
                        args.push(quote! {
                            if let Some(value) = &self.#ident {
                                let value =
                                    ::clap::ValueEnum::to_possible_value(value)
                                    .expect("not a skipped variant");
                                process.arg(#long).args([value.get_name()]);
                            }
                        });
                    // TODO(tailhook) maybe done, but the problem is that
                    //   default_value is a string
                    //} else if let Some(val) = &field.attrs.default_value {
                    //    args.push(quote! {
                    //        if self.#ident != #val {
                    //            process.arg(#long).arg(&self.#ident);
                    //        }
                    //    });
                    } else {
                        args.push(quote! {
                            let value =
                                ::clap::ValueEnum::to_possible_value(
                                    &self.#ident
                                ).expect("not a skipped variant");
                            process.arg(#long).args([value.get_name()]);
                        });
                    }
                }
                FromFlag => {
                    args.push(quote! {
                        if self.#ident {
                            process.arg(#long);
                        }
                    });
                }
            }
        } else if field.attrs.short.is_some() {
            abort!(
                field.ident,
                "only long options and positionals are implemented"
            );
        } else {
            if field.multiple {
                abort!(field.ident, "multiple is not implemented");
            }
            match field.parse.kind {
                FromOccurrences => {
                    abort!(field.ident, "occurrences are not implemented");
                }
                FromStr | FromOsStr | TryFromStr | TryFromOsStr => {
                    if field.optional {
                        args.push(quote! {
                            if let Some(value) = &self.#ident {
                                process.args([value]);
                            }
                        });
                    } else if let Some(val) = &field.attrs.default_value {
                        args.push(quote! {
                            if self.#ident != #val {
                                process.args([&self.#ident]);
                            }
                        });
                    } else {
                        args.push(quote! {
                            process.args([&self.#ident]);
                        });
                    }
                }
                FromFlag => {
                    abort!(field.ident, "positional from_flag are not implemented");
                }
                ValueEnum => {
                    if field.optional {
                        args.push(quote! {
                            if let Some(value) = &self.#ident {
                                let value =
                                    ::clap::ValueEnum::to_possible_value(value)
                                    .expect("not a skipped variant");
                                process.args([value.get_name()]);
                            }
                        });
                    } else if let Some(val) = &field.attrs.default_value {
                        args.push(quote! {
                            if self.#ident != #val {
                                let value =
                                    ::clap::ValueEnum::to_possible_value(
                                        &self.#ident
                                    ).expect("not a skipped variant");
                                process.args([value.get_name()]);
                            }
                        });
                    } else {
                        args.push(quote! {
                            let value =
                                ::clap::ValueEnum::to_possible_value(
                                    &self.#ident
                                ).expect("not a skipped variant");
                            process.args([value.get_name()]);
                        });
                    }
                }
            }
        }
    }

    quote! {
        impl #impl_gen crate::process::IntoArgs
            for &'_ #ident #ty_gen #where_cl
        {
            fn add_args(self, process: &mut crate::process::Native) {
                #(#args)*
            }
        }
    }
}
