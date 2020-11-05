use crate::pdl::{Command, DataType, Domain, Event, Item, Param, TypeDef, Variant};
use heck::CamelCase;
use proc_macro2::TokenStream;
use quote::quote;
use std::slice::Iter;

pub struct DomainDataTypeIter<'a> {
    types: Iter<'a, TypeDef<'a>>,
    commands: Iter<'a, Command<'a>>,
    events: Iter<'a, Event<'a>>,
}

impl<'a> Iterator for DomainDataTypeIter<'a> {
    type Item = DomainDatatype<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(ty) = self.types.next() {
            return Some(DomainDatatype::Type(ty));
        }
        if let Some(cmd) = self.commands.next() {
            return Some(DomainDatatype::Commnad(cmd));
        }
        if let Some(ev) = self.events.next() {
            return Some(DomainDatatype::Event(ev));
        }
        None
    }
}

impl<'a> IntoIterator for &'a Domain<'a> {
    type Item = DomainDatatype<'a>;
    type IntoIter = DomainDataTypeIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        DomainDataTypeIter {
            types: self.types.iter(),
            commands: self.commands.iter(),
            events: self.events.iter(),
        }
    }
}

pub enum DomainDatatype<'a> {
    Type(&'a TypeDef<'a>),
    Commnad(&'a Command<'a>),
    Event(&'a Event<'a>),
}

impl<'a> DomainDatatype<'a> {
    pub fn is_type(&self) -> bool {
        matches!(self, DomainDatatype::Type(_))
    }

    pub fn is_command(&self) -> bool {
        matches!(self, DomainDatatype::Commnad(_))
    }

    pub fn is_event(&self) -> bool {
        matches!(self, DomainDatatype::Event(_))
    }

    pub fn is_substituted(&self) -> bool {
        ["SessionID"].contains(&self.name())
    }

    pub fn type_description_tokens(&self, domain_name: &str) -> TokenStream {
        let base_url = "https://chromedevtools.github.io/devtools-protocol/tot/";

        let url = match self {
            DomainDatatype::Type(ty) => format!("{}{}/#type-{}", base_url, domain_name, ty.name()),
            DomainDatatype::Commnad(cmd) => {
                format!("{}{}/#method-{}", base_url, domain_name, cmd.name())
            }
            DomainDatatype::Event(ev) => {
                format!("{}{}/#event-{}", base_url, domain_name, ev.name())
            }
        };

        if let Some(desc) = self.description() {
            let desc = format!("{}\n[{}]({})", desc, self.name(), url);
            quote! {
                #[doc = #desc]
            }
        } else {
            TokenStream::default()
        }
    }

    pub fn ident_name(&self) -> String {
        match self {
            DomainDatatype::Type(_ty) => self.name().to_camel_case(),
            DomainDatatype::Commnad(cmd) => format!("{}Params", cmd.name().to_camel_case()),
            DomainDatatype::Event(event) => format!("Event{}", event.name().to_camel_case()),
        }
    }

    pub fn params(&self) -> impl Iterator<Item = &'a Param<'a>> + 'a {
        match self {
            DomainDatatype::Type(ty) => {
                if let Some(Item::Properties(ref params)) = ty.item {
                    params.iter()
                } else {
                    [].iter()
                }
            }
            DomainDatatype::Commnad(cmd) => cmd.parameters.iter(),
            DomainDatatype::Event(ev) => ev.parameters.iter(),
        }
    }

    pub fn as_enum(&self) -> Option<&Vec<Variant>> {
        match self {
            DomainDatatype::Type(ty) => {
                if let Some(Item::Enum(ref vars)) = ty.item {
                    Some(vars)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn raw_name(&self) -> &'a str {
        match self {
            DomainDatatype::Type(ty) => ty.raw_name.as_ref(),
            DomainDatatype::Commnad(cmd) => cmd.raw_name.as_ref(),
            DomainDatatype::Event(ev) => ev.raw_name.as_ref(),
        }
    }
}

impl<'a> DataType for DomainDatatype<'a> {
    fn is_circular_dep(&self) -> bool {
        match self {
            DomainDatatype::Type(inner) => inner.is_circular_dep(),
            DomainDatatype::Commnad(inner) => inner.is_circular_dep(),
            DomainDatatype::Event(inner) => inner.is_circular_dep(),
        }
    }

    fn is_experimental(&self) -> bool {
        match self {
            DomainDatatype::Type(inner) => inner.is_experimental(),
            DomainDatatype::Commnad(inner) => inner.is_experimental(),
            DomainDatatype::Event(inner) => inner.is_experimental(),
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            DomainDatatype::Type(inner) => inner.description(),
            DomainDatatype::Commnad(inner) => inner.description(),
            DomainDatatype::Event(inner) => inner.description(),
        }
    }

    fn name(&self) -> &str {
        match self {
            DomainDatatype::Type(inner) => inner.name(),
            DomainDatatype::Commnad(inner) => inner.name(),
            DomainDatatype::Event(inner) => inner.name(),
        }
    }

    fn is_deprecated(&self) -> bool {
        match self {
            DomainDatatype::Type(inner) => inner.is_deprecated(),
            DomainDatatype::Commnad(inner) => inner.is_deprecated(),
            DomainDatatype::Event(inner) => inner.is_deprecated(),
        }
    }
}
