use heck::*;
use proc_macro2::{Ident, TokenStream};
use quote::quote;

use crate::build::types::{DomainDatatype, FieldDefinition};
use crate::build::SerdeSupport;

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

    fn mandatory<'a>(&'a self) -> impl Iterator<Item = &'a FieldDefinition> + 'a {
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

        // clippy allows up to 7 arguments
        // let's limit this to 4, since all fields are public usual struct init is
        // always possible
        let mandatory_count = self.mandatory().count();

        if mandatory_count <= 4 {
            // add new fn

            let optionals = self
                .fields
                .iter()
                .filter(|(_, f)| f.optional)
                .map(|(_, f)| &f.name);

            let mut param_name = vec![];
            let mut param_ty = vec![];
            let mut assign = vec![];

            for field in self.mandatory() {
                let field_name = &field.name;
                param_name.push(field_name);
                param_ty.push(field.ty.param_type_def());
                if field.ty.is_vec {
                    assign.push(quote! {#field_name});
                } else if field.ty.needs_box {
                    assign.push(quote! {#field_name : Box::new(#field_name.into())});
                } else {
                    assign.push(quote! {#field_name : #field_name.into()});
                }
            }

            stream.extend(quote! {
                impl #name {

                    pub fn new(#(#param_name : #param_ty),*) -> Self {
                        Self {
                          #(#assign,)*
                          #(#optionals : None),*
                        }
                    }
                }
            })
        }

        stream
    }
}
