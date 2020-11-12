use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::build::types::FieldDefinition;

const MIN_FIELDS: usize = 4;

pub struct Builder {
    pub fields: Vec<(TokenStream, FieldDefinition)>,
    pub name: Ident,
}

impl Builder {
    pub fn new(name: Ident) -> Self {
        Self {
            name,
            fields: vec![],
        }
    }

    pub fn has_mandatory_types(&self) -> bool {
        self.mandatory().any(|f| !f.optional)
    }

    fn mandatory(&self) -> impl Iterator<Item = &FieldDefinition> + '_ {
        self.fields
            .iter()
            .filter(|(_, f)| !f.optional)
            .map(|(_, f)| f)
    }

    pub fn generate_struct_def(&self) -> TokenStream {
        let name = &self.name;
        let field_definitions = self.fields.iter().map(|(def, _)| def);
        quote! {
             pub struct #name {
                #(#field_definitions),*
             }
        }
    }

    pub fn generate_impl(&self) -> TokenStream {
        let mut stream = TokenStream::default();
        if self.fields.is_empty() {
            return stream;
        }

        let name = &self.name;

        let optionals = self
            .fields
            .iter()
            .filter(|(_, f)| f.optional)
            .map(|(_, f)| &f.name);

        let mut mandatory_param_name = vec![];
        let mut mandatory_param_ty = vec![];
        let mut assign = vec![];

        for field in self.mandatory() {
            let field_name = &field.name;
            mandatory_param_name.push(field_name);
            mandatory_param_ty.push(field.ty.param_type_def());
            if field.ty.is_vec {
                assign.push(quote! {#field_name});
            } else if field.ty.needs_box {
                assign.push(quote! {#field_name : Box::new(#field_name.into())});
            } else {
                assign.push(quote! {#field_name : #field_name.into()});
            }
        }

        // clippy allows up to 7 arguments: https://rust-lang.github.io/rust-clippy/master/#too_many_arguments
        // let's limit this to 4, since all fields are public usual struct init is
        // always possible
        if mandatory_param_name.len() <= 4 {
            stream.extend(quote! {
                impl #name {
                    pub fn new(#(#mandatory_param_name : #mandatory_param_ty),*) -> Self {
                        Self {
                          #(#assign,)*
                          #(#optionals : None),*
                        }
                    }
                }
            })
        }

        if self.fields.len() < MIN_FIELDS {
            // don't create builder for structs with less than `MIN_FIELDS` fields
            return stream;
        }

        let builder = format_ident!("{}Builder", self.name);

        let mut setters = TokenStream::default();
        let mut names = vec![];
        let mut builder_type_defs = TokenStream::default();
        let mut build_fn_assigns = TokenStream::default();

        for field in self.fields.iter().map(|(_, f)| f) {
            let field_name = &field.name;
            names.push(field_name);
            let builder_ty = field.ty.builder_type();
            builder_type_defs.extend(quote! {
                #field_name: Option<#builder_ty>,
            });

            let ty_param = field.ty.param_type_def();
            let assign = if field.ty.is_vec {
                quote! {#field_name}
            } else {
                quote! {#field_name.into()}
            };

            setters.extend(quote! {
                pub fn #field_name(mut self, #field_name : #ty_param ) -> Self {
                    self.#field_name = Some(#assign);
                    self
                }
            });
            // TODO special case for vec

            // mappings for the `build` fn
            if field.optional {
                if field.ty.needs_box {
                    build_fn_assigns.extend(quote! {
                        #field_name : self.#field_name.map(Box::new),
                    })
                } else {
                    build_fn_assigns.extend(quote! {
                        #field_name : self.#field_name,
                    })
                }
            } else if field.ty.needs_box {
                build_fn_assigns.extend(
                        quote!{
                            #field_name : Box::new(self.#field_name.ok_or_else(||std::stringify!("Field `{}` is mandatory.", std::stringify!(#field_name))))?,
                        }
                    )
            } else {
                build_fn_assigns.extend(
                        quote!{
                            #field_name : self.#field_name.ok_or_else(||format!("Field `{}` is mandatory.", std::stringify!(#field_name)))?,
                        }
                    )
            }
        }

        let build_fn = if mandatory_param_name.is_empty() {
            quote! {
                pub fn build(self) -> #name {
                    #name {
                        #build_fn_assigns
                    }
                }
            }
        } else {
            quote! {
                pub fn build(self) -> Result<#name, String> {
                    Ok(#name {
                        #build_fn_assigns
                    })
                }
            }
        };

        stream.extend(quote! {

               impl #name {
                    pub fn builder() -> #builder {
                        #builder::default()
                    }
               }

               #[derive(Default, Clone)]
               pub struct #builder {
                    #builder_type_defs
               }

               impl #builder {
                    #setters
                    #build_fn
               }
        });

        stream
    }
}
