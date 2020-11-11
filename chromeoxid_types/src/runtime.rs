use serde::{Deserialize, Serialize};
#[doc = "Id of an execution context.\n[ExecutionContextId](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-ExecutionContextId)"]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionContextId(u32);
impl ExecutionContextId {
    pub const IDENTIFIER: &'static str = "Runtime.ExecutionContextId";
}

#[doc = "Unique script identifier.\n[ScriptId](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-ScriptId)"]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ScriptId(String);
impl ScriptId {
    pub const IDENTIFIER: &'static str = "Runtime.ScriptId";
}

impl AsRef<str> for ScriptId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[doc = "Unique object identifier.\n[RemoteObjectId](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-RemoteObjectId)"]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct RemoteObjectId(String);
impl RemoteObjectId {
    pub const IDENTIFIER: &'static str = "Runtime.RemoteObjectId";
}

impl AsRef<str> for RemoteObjectId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[doc = "Primitive value which cannot be JSON-stringified. Includes values `-0`, `NaN`, `Infinity`,\n`-Infinity`, and bigint literals.\n[UnserializableValue](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-UnserializableValue)"]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnserializableValue(String);
impl UnserializableValue {
    pub const IDENTIFIER: &'static str = "Runtime.UnserializableValue";
}

impl AsRef<str> for UnserializableValue {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[doc = "Mirror object referencing original JavaScript object.\n[RemoteObject](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-RemoteObject)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteObject {
    #[doc = "Object type."]
    pub r#type: RemoteObjectType,
    #[doc = "Object subtype hint. Specified for `object` or `wasm` type values only."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<RemoteObjectSubtype>,
    #[doc = "Object class (constructor) name. Specified for `object` type values only."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[doc = "Remote object value in case of primitive values or JSON values (if it was requested)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[doc = "Primitive value which can not be JSON-stringified does not have `value`, but gets this\nproperty."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unserializable_value: Option<UnserializableValue>,
    #[doc = "String representation of the object."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[doc = "Unique object identifier (for non-primitive values)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<RemoteObjectId>,
    #[doc = "Preview containing abbreviated property values. Specified for `object` type values only."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<ObjectPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_preview: Option<CustomPreview>,
}
impl RemoteObject {
    pub fn new(r#type: RemoteObjectType) -> RemoteObject {
        Self {
            r#type,
            subtype: Default::default(),
            class_name: Default::default(),
            value: Default::default(),
            unserializable_value: Default::default(),
            description: Default::default(),
            object_id: Default::default(),
            preview: Default::default(),
            custom_preview: Default::default(),
        }
    }
}
#[doc = "Object type."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteObjectType {
    Object,
    Function,
    Undefined,
    String,
    Number,
    Boolean,
    Symbol,
    Bigint,
    Wasm,
}
impl RemoteObjectType {
    pub fn as_str(&self) -> &'static str {
        match self {
            RemoteObjectType::Object => "object",
            RemoteObjectType::Function => "function",
            RemoteObjectType::Undefined => "undefined",
            RemoteObjectType::String => "string",
            RemoteObjectType::Number => "number",
            RemoteObjectType::Boolean => "boolean",
            RemoteObjectType::Symbol => "symbol",
            RemoteObjectType::Bigint => "bigint",
            RemoteObjectType::Wasm => "wasm",
        }
    }
}
impl ::std::str::FromStr for RemoteObjectType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "object" => Ok(RemoteObjectType::Object),
            "function" => Ok(RemoteObjectType::Function),
            "undefined" => Ok(RemoteObjectType::Undefined),
            "string" => Ok(RemoteObjectType::String),
            "number" => Ok(RemoteObjectType::Number),
            "boolean" => Ok(RemoteObjectType::Boolean),
            "symbol" => Ok(RemoteObjectType::Symbol),
            "bigint" => Ok(RemoteObjectType::Bigint),
            "wasm" => Ok(RemoteObjectType::Wasm),
            _ => Err(s.to_string()),
        }
    }
}
#[doc = "Object subtype hint. Specified for `object` or `wasm` type values only."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteObjectSubtype {
    Array,
    Null,
    Node,
    Regexp,
    Date,
    Map,
    Set,
    Weakmap,
    Weakset,
    Iterator,
    Generator,
    Error,
    Proxy,
    Promise,
    Typedarray,
    Arraybuffer,
    Dataview,
    I32,
    I64,
    F32,
    F64,
    V128,
    Externref,
}
impl RemoteObjectSubtype {
    pub fn as_str(&self) -> &'static str {
        match self {
            RemoteObjectSubtype::Array => "array",
            RemoteObjectSubtype::Null => "null",
            RemoteObjectSubtype::Node => "node",
            RemoteObjectSubtype::Regexp => "regexp",
            RemoteObjectSubtype::Date => "date",
            RemoteObjectSubtype::Map => "map",
            RemoteObjectSubtype::Set => "set",
            RemoteObjectSubtype::Weakmap => "weakmap",
            RemoteObjectSubtype::Weakset => "weakset",
            RemoteObjectSubtype::Iterator => "iterator",
            RemoteObjectSubtype::Generator => "generator",
            RemoteObjectSubtype::Error => "error",
            RemoteObjectSubtype::Proxy => "proxy",
            RemoteObjectSubtype::Promise => "promise",
            RemoteObjectSubtype::Typedarray => "typedarray",
            RemoteObjectSubtype::Arraybuffer => "arraybuffer",
            RemoteObjectSubtype::Dataview => "dataview",
            RemoteObjectSubtype::I32 => "i32",
            RemoteObjectSubtype::I64 => "i64",
            RemoteObjectSubtype::F32 => "f32",
            RemoteObjectSubtype::F64 => "f64",
            RemoteObjectSubtype::V128 => "v128",
            RemoteObjectSubtype::Externref => "externref",
        }
    }
}
impl ::std::str::FromStr for RemoteObjectSubtype {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "array" => Ok(RemoteObjectSubtype::Array),
            "null" => Ok(RemoteObjectSubtype::Null),
            "node" => Ok(RemoteObjectSubtype::Node),
            "regexp" => Ok(RemoteObjectSubtype::Regexp),
            "date" => Ok(RemoteObjectSubtype::Date),
            "map" => Ok(RemoteObjectSubtype::Map),
            "set" => Ok(RemoteObjectSubtype::Set),
            "weakmap" => Ok(RemoteObjectSubtype::Weakmap),
            "weakset" => Ok(RemoteObjectSubtype::Weakset),
            "iterator" => Ok(RemoteObjectSubtype::Iterator),
            "generator" => Ok(RemoteObjectSubtype::Generator),
            "error" => Ok(RemoteObjectSubtype::Error),
            "proxy" => Ok(RemoteObjectSubtype::Proxy),
            "promise" => Ok(RemoteObjectSubtype::Promise),
            "typedarray" => Ok(RemoteObjectSubtype::Typedarray),
            "arraybuffer" => Ok(RemoteObjectSubtype::Arraybuffer),
            "dataview" => Ok(RemoteObjectSubtype::Dataview),
            "i32" => Ok(RemoteObjectSubtype::I32),
            "i64" => Ok(RemoteObjectSubtype::I64),
            "f32" => Ok(RemoteObjectSubtype::F32),
            "f64" => Ok(RemoteObjectSubtype::F64),
            "v128" => Ok(RemoteObjectSubtype::V128),
            "externref" => Ok(RemoteObjectSubtype::Externref),
            _ => Err(s.to_string()),
        }
    }
}
impl RemoteObject {
    pub const IDENTIFIER: &'static str = "Runtime.RemoteObject";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomPreview {
    #[doc = "The JSON-stringified result of formatter.header(object, config) call.\nIt contains json ML array that represents RemoteObject."]
    pub header: String,
    #[doc = "If formatter returns true as a result of formatter.hasBody call then bodyGetterId will\ncontain RemoteObjectId for the function that returns result of formatter.body(object, config) call.\nThe result value is json ML array."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_getter_id: Option<RemoteObjectId>,
}
impl CustomPreview {
    pub fn new(header: String) -> CustomPreview {
        Self {
            header,
            body_getter_id: Default::default(),
        }
    }
}
impl CustomPreview {
    pub const IDENTIFIER: &'static str = "Runtime.CustomPreview";
}

#[doc = "Object containing abbreviated remote object value.\n[ObjectPreview](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-ObjectPreview)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPreview {
    #[doc = "Object type."]
    pub r#type: ObjectPreviewType,
    #[doc = "Object subtype hint. Specified for `object` type values only."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<ObjectPreviewSubtype>,
    #[doc = "String representation of the object."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[doc = "True iff some of the properties or entries of the original object did not fit."]
    pub overflow: bool,
    #[doc = "List of the properties."]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyPreview>,
    #[doc = "List of the entries. Specified for `map` and `set` subtype values only."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<EntryPreview>>,
}
impl ObjectPreview {
    pub fn new(
        r#type: ObjectPreviewType,
        overflow: bool,
        properties: Vec<PropertyPreview>,
    ) -> ObjectPreview {
        Self {
            r#type,
            overflow,
            properties,
            subtype: Default::default(),
            description: Default::default(),
            entries: Default::default(),
        }
    }
}
#[doc = "Object type."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectPreviewType {
    Object,
    Function,
    Undefined,
    String,
    Number,
    Boolean,
    Symbol,
    Bigint,
}
impl ObjectPreviewType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObjectPreviewType::Object => "object",
            ObjectPreviewType::Function => "function",
            ObjectPreviewType::Undefined => "undefined",
            ObjectPreviewType::String => "string",
            ObjectPreviewType::Number => "number",
            ObjectPreviewType::Boolean => "boolean",
            ObjectPreviewType::Symbol => "symbol",
            ObjectPreviewType::Bigint => "bigint",
        }
    }
}
impl ::std::str::FromStr for ObjectPreviewType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "object" => Ok(ObjectPreviewType::Object),
            "function" => Ok(ObjectPreviewType::Function),
            "undefined" => Ok(ObjectPreviewType::Undefined),
            "string" => Ok(ObjectPreviewType::String),
            "number" => Ok(ObjectPreviewType::Number),
            "boolean" => Ok(ObjectPreviewType::Boolean),
            "symbol" => Ok(ObjectPreviewType::Symbol),
            "bigint" => Ok(ObjectPreviewType::Bigint),
            _ => Err(s.to_string()),
        }
    }
}
#[doc = "Object subtype hint. Specified for `object` type values only."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectPreviewSubtype {
    Array,
    Null,
    Node,
    Regexp,
    Date,
    Map,
    Set,
    Weakmap,
    Weakset,
    Iterator,
    Generator,
    Error,
}
impl ObjectPreviewSubtype {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObjectPreviewSubtype::Array => "array",
            ObjectPreviewSubtype::Null => "null",
            ObjectPreviewSubtype::Node => "node",
            ObjectPreviewSubtype::Regexp => "regexp",
            ObjectPreviewSubtype::Date => "date",
            ObjectPreviewSubtype::Map => "map",
            ObjectPreviewSubtype::Set => "set",
            ObjectPreviewSubtype::Weakmap => "weakmap",
            ObjectPreviewSubtype::Weakset => "weakset",
            ObjectPreviewSubtype::Iterator => "iterator",
            ObjectPreviewSubtype::Generator => "generator",
            ObjectPreviewSubtype::Error => "error",
        }
    }
}
impl ::std::str::FromStr for ObjectPreviewSubtype {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "array" => Ok(ObjectPreviewSubtype::Array),
            "null" => Ok(ObjectPreviewSubtype::Null),
            "node" => Ok(ObjectPreviewSubtype::Node),
            "regexp" => Ok(ObjectPreviewSubtype::Regexp),
            "date" => Ok(ObjectPreviewSubtype::Date),
            "map" => Ok(ObjectPreviewSubtype::Map),
            "set" => Ok(ObjectPreviewSubtype::Set),
            "weakmap" => Ok(ObjectPreviewSubtype::Weakmap),
            "weakset" => Ok(ObjectPreviewSubtype::Weakset),
            "iterator" => Ok(ObjectPreviewSubtype::Iterator),
            "generator" => Ok(ObjectPreviewSubtype::Generator),
            "error" => Ok(ObjectPreviewSubtype::Error),
            _ => Err(s.to_string()),
        }
    }
}
impl ObjectPreview {
    pub const IDENTIFIER: &'static str = "Runtime.ObjectPreview";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyPreview {
    #[doc = "Property name."]
    pub name: String,
    #[doc = "Object type. Accessor means that the property itself is an accessor property."]
    pub r#type: PropertyPreviewType,
    #[doc = "User-friendly property value string."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[doc = "Nested value preview."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_preview: Option<ObjectPreview>,
    #[doc = "Object subtype hint. Specified for `object` type values only."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<PropertyPreviewSubtype>,
}
impl PropertyPreview {
    pub fn new(name: String, r#type: PropertyPreviewType) -> PropertyPreview {
        Self {
            name,
            r#type,
            value: Default::default(),
            value_preview: Default::default(),
            subtype: Default::default(),
        }
    }
}
#[doc = "Object type. Accessor means that the property itself is an accessor property."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PropertyPreviewType {
    Object,
    Function,
    Undefined,
    String,
    Number,
    Boolean,
    Symbol,
    Accessor,
    Bigint,
}
impl PropertyPreviewType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PropertyPreviewType::Object => "object",
            PropertyPreviewType::Function => "function",
            PropertyPreviewType::Undefined => "undefined",
            PropertyPreviewType::String => "string",
            PropertyPreviewType::Number => "number",
            PropertyPreviewType::Boolean => "boolean",
            PropertyPreviewType::Symbol => "symbol",
            PropertyPreviewType::Accessor => "accessor",
            PropertyPreviewType::Bigint => "bigint",
        }
    }
}
impl ::std::str::FromStr for PropertyPreviewType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "object" => Ok(PropertyPreviewType::Object),
            "function" => Ok(PropertyPreviewType::Function),
            "undefined" => Ok(PropertyPreviewType::Undefined),
            "string" => Ok(PropertyPreviewType::String),
            "number" => Ok(PropertyPreviewType::Number),
            "boolean" => Ok(PropertyPreviewType::Boolean),
            "symbol" => Ok(PropertyPreviewType::Symbol),
            "accessor" => Ok(PropertyPreviewType::Accessor),
            "bigint" => Ok(PropertyPreviewType::Bigint),
            _ => Err(s.to_string()),
        }
    }
}
#[doc = "Object subtype hint. Specified for `object` type values only."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PropertyPreviewSubtype {
    Array,
    Null,
    Node,
    Regexp,
    Date,
    Map,
    Set,
    Weakmap,
    Weakset,
    Iterator,
    Generator,
    Error,
}
impl PropertyPreviewSubtype {
    pub fn as_str(&self) -> &'static str {
        match self {
            PropertyPreviewSubtype::Array => "array",
            PropertyPreviewSubtype::Null => "null",
            PropertyPreviewSubtype::Node => "node",
            PropertyPreviewSubtype::Regexp => "regexp",
            PropertyPreviewSubtype::Date => "date",
            PropertyPreviewSubtype::Map => "map",
            PropertyPreviewSubtype::Set => "set",
            PropertyPreviewSubtype::Weakmap => "weakmap",
            PropertyPreviewSubtype::Weakset => "weakset",
            PropertyPreviewSubtype::Iterator => "iterator",
            PropertyPreviewSubtype::Generator => "generator",
            PropertyPreviewSubtype::Error => "error",
        }
    }
}
impl ::std::str::FromStr for PropertyPreviewSubtype {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "array" => Ok(PropertyPreviewSubtype::Array),
            "null" => Ok(PropertyPreviewSubtype::Null),
            "node" => Ok(PropertyPreviewSubtype::Node),
            "regexp" => Ok(PropertyPreviewSubtype::Regexp),
            "date" => Ok(PropertyPreviewSubtype::Date),
            "map" => Ok(PropertyPreviewSubtype::Map),
            "set" => Ok(PropertyPreviewSubtype::Set),
            "weakmap" => Ok(PropertyPreviewSubtype::Weakmap),
            "weakset" => Ok(PropertyPreviewSubtype::Weakset),
            "iterator" => Ok(PropertyPreviewSubtype::Iterator),
            "generator" => Ok(PropertyPreviewSubtype::Generator),
            "error" => Ok(PropertyPreviewSubtype::Error),
            _ => Err(s.to_string()),
        }
    }
}
impl PropertyPreview {
    pub const IDENTIFIER: &'static str = "Runtime.PropertyPreview";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPreview {
    #[doc = "Preview of the key. Specified for map-like collection entries."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<ObjectPreview>,
    #[doc = "Preview of the value."]
    pub value: ObjectPreview,
}
impl EntryPreview {
    pub fn new(value: ObjectPreview) -> EntryPreview {
        Self {
            value,
            key: Default::default(),
        }
    }
}
impl EntryPreview {
    pub const IDENTIFIER: &'static str = "Runtime.EntryPreview";
}
