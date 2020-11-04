use crate::pdl::*;

/// Generates a domain that contains util and message types
pub(crate) fn generate_util_domain() -> Domain<'static> {
    let types = vec![
        TypeDef {
            description: Some(
                "Chrome DevTools Protocol message sent/read over websocket connection.".into(),
            ),
            experimental: false,
            deprecated: false,
            name: "Message".into(),
            extends: Type::Object,
            item: Some(Item::Properties(vec![
                Param {
                    description: Some("Unique message identifier.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: true,
                    r#type: Type::Integer,
                    name: "id".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
                Param {
                    description: Some(
                        "Session that the message belongs to when using flat access.".into(),
                    ),
                    experimental: false,
                    deprecated: false,
                    optional: true,
                    r#type: Type::Ref("Target.SessionID".into()),
                    name: "sessionId".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
                Param {
                    description: Some("Event or command type.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: true,
                    r#type: Type::Ref("MethodType".into()),
                    name: "method".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
                Param {
                    description: Some("Event or command parameters.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: true,
                    r#type: Type::Any,
                    name: "params".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
                Param {
                    description: Some("Command return values.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: true,
                    r#type: Type::Any,
                    name: "result".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
                Param {
                    description: Some("Error message.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: true,
                    r#type: Type::Ref("Error".into()),
                    name: "error".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
            ])),
            raw_name: Default::default(),
            is_circular_dep: false,
        },
        TypeDef {
            description: Some("Error type.".into()),
            experimental: false,
            deprecated: false,
            name: "Error".into(),
            extends: Type::Object,
            item: Some(Item::Properties(vec![
                Param {
                    description: Some("Error code.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: false,
                    r#type: Type::Integer,
                    name: "code".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
                Param {
                    description: Some("Error message.".into()),
                    experimental: false,
                    deprecated: false,
                    optional: false,
                    r#type: Type::String,
                    name: "message".into(),
                    raw_name: Default::default(),
                    is_circular_dep: false,
                },
            ])),
            raw_name: Default::default(),
            is_circular_dep: false,
        },
    ];

    Domain {
        description: Some("Chrome DevTool Types".into()),
        experimental: false,
        deprecated: false,
        name: "cdp".into(),
        dependencies: vec![],
        types,
        commands: vec![],
        events: vec![],
    }
}
