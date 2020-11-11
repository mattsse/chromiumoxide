use crate::{Command, ExecutionContextId, FrameId, Method, RemoteObject, RemoteObjectId};
use serde::{Deserialize, Serialize};

#[doc = "Returns the root DOM node (and optionally the subtree) to the caller.\n[getDocument](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-getDocument)"]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDocumentParams {
    #[doc = "The maximum depth at which children should be retrieved, defaults to 1. Use -1 for the\nentire subtree or provide an integer larger than 0."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[doc = "Whether or not iframes and shadow roots should be traversed when returning the subtree\n(default is false)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pierce: Option<bool>,
}
impl GetDocumentParams {
    pub const IDENTIFIER: &'static str = "DOM.getDocument";
}
impl Method for GetDocumentParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}
#[doc = "Returns the root DOM node (and optionally the subtree) to the caller.\n[getDocument](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-getDocument)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDocumentReturns {
    #[doc = "Resulting node."]
    pub root: Node,
}
impl GetDocumentReturns {
    pub fn new(root: Node) -> GetDocumentReturns {
        Self { root }
    }
}
impl Command for GetDocumentParams {
    type Response = GetDocumentReturns;
}

#[doc = "DOM interaction is implemented in terms of mirror objects that represent the actual DOM nodes.\nDOMNode is a base node mirror type.\n[Node](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#type-Node)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    #[doc = "Node identifier that is passed into the rest of the DOM messages as the `nodeId`. Backend\nwill only push node with given `id` once. It is aware of all requested nodes and will only\nfire DOM events for nodes known to the client."]
    pub node_id: NodeId,
    #[doc = "The id of the parent node if any."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<NodeId>,
    #[doc = "The BackendNodeId for this node."]
    pub backend_node_id: BackendNodeId,
    #[doc = "`Node`'s nodeType."]
    pub node_type: u32,
    #[doc = "`Node`'s nodeName."]
    pub node_name: String,
    #[doc = "`Node`'s localName."]
    pub local_name: String,
    #[doc = "`Node`'s nodeValue."]
    pub node_value: String,
    #[doc = "Child count for `Container` nodes."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_node_count: Option<u32>,
    #[doc = "Child nodes of this node when requested with children."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Node>>,
    #[doc = "Attributes of the `Element` node in the form of flat array `[name1, value1, name2, value2]`."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<String>>,
    #[doc = "Document URL that `Document` or `FrameOwner` node points to."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_url: Option<String>,
    #[doc = "Base URL that `Document` or `FrameOwner` node uses for URL completion."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[doc = "`DocumentType`'s publicId."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_id: Option<String>,
    #[doc = "`DocumentType`'s systemId."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_id: Option<String>,
    #[doc = "`DocumentType`'s internalSubset."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_subset: Option<String>,
    #[doc = "`Document`'s XML version in case of XML documents."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xml_version: Option<String>,
    #[doc = "`Attr`'s name."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[doc = "`Attr`'s value."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[doc = "Pseudo element type for this node."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pseudo_type: Option<PseudoType>,
    #[doc = "Shadow root type."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_root_type: Option<ShadowRootType>,
    #[doc = "Frame ID for frame owner elements."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<FrameId>,
    #[doc = "Content document for frame owner elements."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_document: Option<Box<Node>>,
    #[doc = "Shadow root list for given element host."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_roots: Option<Vec<Node>>,
    #[doc = "Content document fragment for template elements."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_content: Option<Box<Node>>,
    #[doc = "Pseudo elements associated with this node."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pseudo_elements: Option<Vec<Node>>,
    #[doc = "Import document for the HTMLImport links."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_document: Option<Box<Node>>,
    #[doc = "Distributed nodes for given insertion point."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributed_nodes: Option<Vec<BackendNode>>,
    #[doc = "Whether the node is SVG."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_svg: Option<bool>,
}
impl Node {
    pub fn new(
        node_id: NodeId,
        backend_node_id: BackendNodeId,
        node_type: u32,
        node_name: String,
        local_name: String,
        node_value: String,
    ) -> Node {
        Self {
            node_id,
            backend_node_id,
            node_type,
            node_name,
            local_name,
            node_value,
            parent_id: Default::default(),
            child_node_count: Default::default(),
            children: Default::default(),
            attributes: Default::default(),
            document_url: Default::default(),
            base_url: Default::default(),
            public_id: Default::default(),
            system_id: Default::default(),
            internal_subset: Default::default(),
            xml_version: Default::default(),
            name: Default::default(),
            value: Default::default(),
            pseudo_type: Default::default(),
            shadow_root_type: Default::default(),
            frame_id: Default::default(),
            content_document: Default::default(),
            shadow_roots: Default::default(),
            template_content: Default::default(),
            pseudo_elements: Default::default(),
            imported_document: Default::default(),
            distributed_nodes: Default::default(),
            is_svg: Default::default(),
        }
    }
}
impl Node {
    pub const IDENTIFIER: &'static str = "DOM.Node";
}

#[doc = "Unique DOM node identifier.\n[NodeId](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#type-NodeId)"]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct NodeId(u32);
impl NodeId {
    pub const IDENTIFIER: &'static str = "DOM.NodeId";
}

#[doc = "Unique DOM node identifier used to reference a node that may not have been pushed to the\nfront-end.\n[BackendNodeId](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#type-BackendNodeId)"]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct BackendNodeId(u32);
impl BackendNodeId {
    pub const IDENTIFIER: &'static str = "DOM.BackendNodeId";
}

#[doc = "Backend node with a friendly name.\n[BackendNode](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#type-BackendNode)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendNode {
    #[doc = "`Node`'s nodeType."]
    pub node_type: u32,
    #[doc = "`Node`'s nodeName."]
    pub node_name: String,
    pub backend_node_id: BackendNodeId,
}
impl BackendNode {
    pub fn new(node_type: u32, node_name: String, backend_node_id: BackendNodeId) -> BackendNode {
        Self {
            node_type,
            node_name,
            backend_node_id,
        }
    }
}
impl BackendNode {
    pub const IDENTIFIER: &'static str = "DOM.BackendNode";
}

#[doc = "Shadow root type."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShadowRootType {
    UserAgent,
    Open,
    Closed,
}
impl ShadowRootType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShadowRootType::UserAgent => "user-agent",
            ShadowRootType::Open => "open",
            ShadowRootType::Closed => "closed",
        }
    }
}
impl ::std::str::FromStr for ShadowRootType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user-agent" => Ok(ShadowRootType::UserAgent),
            "open" => Ok(ShadowRootType::Open),
            "closed" => Ok(ShadowRootType::Closed),
            _ => Err(s.to_string()),
        }
    }
}

#[doc = "Pseudo element type."]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PseudoType {
    FirstLine,
    FirstLetter,
    Before,
    After,
    Marker,
    Backdrop,
    Selection,
    TargetText,
    FirstLineInherited,
    Scrollbar,
    ScrollbarThumb,
    ScrollbarButton,
    ScrollbarTrack,
    ScrollbarTrackPiece,
    ScrollbarCorner,
    Resizer,
    InputListButton,
}
impl PseudoType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PseudoType::FirstLine => "first-line",
            PseudoType::FirstLetter => "first-letter",
            PseudoType::Before => "before",
            PseudoType::After => "after",
            PseudoType::Marker => "marker",
            PseudoType::Backdrop => "backdrop",
            PseudoType::Selection => "selection",
            PseudoType::TargetText => "target-text",
            PseudoType::FirstLineInherited => "first-line-inherited",
            PseudoType::Scrollbar => "scrollbar",
            PseudoType::ScrollbarThumb => "scrollbar-thumb",
            PseudoType::ScrollbarButton => "scrollbar-button",
            PseudoType::ScrollbarTrack => "scrollbar-track",
            PseudoType::ScrollbarTrackPiece => "scrollbar-track-piece",
            PseudoType::ScrollbarCorner => "scrollbar-corner",
            PseudoType::Resizer => "resizer",
            PseudoType::InputListButton => "input-list-button",
        }
    }
}
impl ::std::str::FromStr for PseudoType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "first-line" => Ok(PseudoType::FirstLine),
            "first-letter" => Ok(PseudoType::FirstLetter),
            "before" => Ok(PseudoType::Before),
            "after" => Ok(PseudoType::After),
            "marker" => Ok(PseudoType::Marker),
            "backdrop" => Ok(PseudoType::Backdrop),
            "selection" => Ok(PseudoType::Selection),
            "target-text" => Ok(PseudoType::TargetText),
            "first-line-inherited" => Ok(PseudoType::FirstLineInherited),
            "scrollbar" => Ok(PseudoType::Scrollbar),
            "scrollbar-thumb" => Ok(PseudoType::ScrollbarThumb),
            "scrollbar-button" => Ok(PseudoType::ScrollbarButton),
            "scrollbar-track" => Ok(PseudoType::ScrollbarTrack),
            "scrollbar-track-piece" => Ok(PseudoType::ScrollbarTrackPiece),
            "scrollbar-corner" => Ok(PseudoType::ScrollbarCorner),
            "resizer" => Ok(PseudoType::Resizer),
            "input-list-button" => Ok(PseudoType::InputListButton),
            _ => Err(s.to_string()),
        }
    }
}

#[doc = "Executes `querySelector` on a given node.\n[querySelector](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-querySelector)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySelectorParams {
    #[doc = "Id of the node to query upon."]
    pub node_id: NodeId,
    #[doc = "Selector string."]
    pub selector: String,
}
impl QuerySelectorParams {
    pub fn new(node_id: NodeId, selector: String) -> QuerySelectorParams {
        Self { node_id, selector }
    }
}
impl QuerySelectorParams {
    pub const IDENTIFIER: &'static str = "DOM.querySelector";
}
impl Method for QuerySelectorParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}
#[doc = "Executes `querySelector` on a given node.\n[querySelector](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-querySelector)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySelectorReturns {
    #[doc = "Query selector result."]
    pub node_id: NodeId,
}
impl QuerySelectorReturns {
    pub fn new(node_id: NodeId) -> QuerySelectorReturns {
        Self { node_id }
    }
}
impl Command for QuerySelectorParams {
    type Response = QuerySelectorReturns;
}

#[doc = "Executes `querySelectorAll` on a given node.\n[querySelectorAll](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-querySelectorAll)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySelectorAllParams {
    #[doc = "Id of the node to query upon."]
    pub node_id: NodeId,
    #[doc = "Selector string."]
    pub selector: String,
}
impl QuerySelectorAllParams {
    pub fn new(node_id: NodeId, selector: String) -> QuerySelectorAllParams {
        Self { node_id, selector }
    }
}
impl QuerySelectorAllParams {
    pub const IDENTIFIER: &'static str = "DOM.querySelectorAll";
}
impl Method for QuerySelectorAllParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}
#[doc = "Executes `querySelectorAll` on a given node.\n[querySelectorAll](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-querySelectorAll)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySelectorAllReturns {
    #[doc = "Query selector result."]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub node_ids: Vec<NodeId>,
}
impl QuerySelectorAllReturns {
    pub fn new(node_ids: Vec<NodeId>) -> QuerySelectorAllReturns {
        Self { node_ids }
    }
}
impl Command for QuerySelectorAllParams {
    type Response = QuerySelectorAllReturns;
}

#[doc = "Resolves the JavaScript node object for a given NodeId or BackendNodeId.\n[resolveNode](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-resolveNode)"]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveNodeParams {
    #[doc = "Id of the node to resolve."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[doc = "Backend identifier of the node to resolve."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<BackendNodeId>,
    #[doc = "Symbolic group name that can be used to release multiple objects."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_group: Option<String>,
    #[doc = "Execution context in which to resolve the node."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_context_id: Option<ExecutionContextId>,
}
impl ResolveNodeParams {
    pub const IDENTIFIER: &'static str = "DOM.resolveNode";

    pub fn with_backend_node(backend_node_id: BackendNodeId) -> Self {
        let mut params = Self::default();
        params.backend_node_id = Some(backend_node_id);
        params
    }
}
impl Method for ResolveNodeParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}
#[doc = "Resolves the JavaScript node object for a given NodeId or BackendNodeId.\n[resolveNode](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-resolveNode)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveNodeReturns {
    #[doc = "JavaScript object wrapper for given node."]
    pub object: RemoteObject,
}
impl ResolveNodeReturns {
    pub fn new(object: RemoteObject) -> ResolveNodeReturns {
        Self { object }
    }
}
impl Command for ResolveNodeParams {
    type Response = ResolveNodeReturns;
}

#[doc = "Describes node given its id, does not require domain to be enabled. Does not start tracking any\nobjects, can be used for automation.\n[describeNode](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-describeNode)"]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeNodeParams {
    #[doc = "Identifier of the node."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[doc = "Identifier of the backend node."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<BackendNodeId>,
    #[doc = "JavaScript object id of the node wrapper."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<RemoteObjectId>,
    #[doc = "The maximum depth at which children should be retrieved, defaults to 1. Use -1 for the\nentire subtree or provide an integer larger than 0."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<i64>,
    #[doc = "Whether or not iframes and shadow roots should be traversed when returning the subtree\n(default is false)."]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pierce: Option<bool>,
}

impl DescribeNodeParams {
    pub fn with_depth(depth: i64) -> Self {
        let mut params = Self::default();
        params.depth = Some(depth);
        params
    }

    pub fn with_node_id(node_id: NodeId) -> Self {
        let mut params = Self::default();
        params.node_id = Some(node_id);
        params
    }

    pub fn with_node_id_and_depth(node_id: NodeId, depth: i64) -> Self {
        let mut params = Self::with_depth(depth);
        params.node_id = Some(node_id);
        params
    }
}

impl DescribeNodeParams {
    pub const IDENTIFIER: &'static str = "DOM.describeNode";
}
impl Method for DescribeNodeParams {
    fn identifier(&self) -> ::std::borrow::Cow<'static, str> {
        Self::IDENTIFIER.into()
    }
}
#[doc = "Describes node given its id, does not require domain to be enabled. Does not start tracking any\nobjects, can be used for automation.\n[describeNode](https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-describeNode)"]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeNodeReturns {
    #[doc = "Node description."]
    pub node: Node,
}
impl DescribeNodeReturns {
    pub fn new(node: Node) -> DescribeNodeReturns {
        Self { node }
    }
}
impl Command for DescribeNodeParams {
    type Response = DescribeNodeReturns;
}
