use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use chromiumoxide_types::{ClickOptions, Command, CommandResponse};
use futures::{Future, FutureExt, Stream, future};

use chromiumoxide_cdp::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, GetBoxModelParams, GetContentQuadsParams,
    GetFrameOwnerParams, Node, NodeId, QuerySelectorAllParams, QuerySelectorParams,
    ResolveNodeParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, FrameId, Viewport,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::SessionId;
use chromiumoxide_cdp::cdp::js_protocol::runtime::{
    CallArgument, CallFunctionOnParams, CallFunctionOnReturns, ExceptionDetails,
    GetPropertiesParams, PropertyDescriptor, ReleaseObjectParams, RemoteObject, RemoteObjectId,
    RemoteObjectSubtype, RemoteObjectType,
};

use crate::error::{CdpError, Result};
use crate::handler::PageInner;
use crate::layout::{BoundingBox, BoxModel, ElementQuad, Point};
use crate::utils;

/// Represents a [DOM Element](https://developer.mozilla.org/en-US/docs/Web/API/Element).
#[derive(Debug, Clone)]
pub struct Element {
    /// The Unique object identifier
    pub remote_object_id: RemoteObjectId,
    /// Identifier of the backend node.
    pub backend_node_id: BackendNodeId,
    /// The document-scoped node identifier, when this handle was created from
    /// an enabled DOM document.
    ///
    /// Frame-scoped selectors are object-based and therefore intentionally do
    /// not manufacture a `NodeId`.
    pub node_id: Option<NodeId>,
    frame_id: FrameId,
    session_id: SessionId,
    tab: Arc<PageInner>,
}

impl Element {
    pub(crate) async fn new(
        tab: Arc<PageInner>,
        node_id: NodeId,
        frame_id: FrameId,
    ) -> Result<Self> {
        let session_id = tab.session_id().clone();
        Self::from_node_in_session(tab, node_id, frame_id, session_id).await
    }

    async fn from_node_in_session(
        tab: Arc<PageInner>,
        node_id: NodeId,
        frame_id: FrameId,
        session_id: SessionId,
    ) -> Result<Self> {
        let backend_node_id = tab
            .execute_with_session(
                DescribeNodeParams::builder()
                    .node_id(node_id)
                    .depth(100)
                    .build(),
                &session_id,
            )
            .await?
            .result
            .node
            .backend_node_id;

        let resp = tab
            .execute_with_session(
                ResolveNodeParams::builder()
                    .backend_node_id(backend_node_id)
                    .build(),
                &session_id,
            )
            .await?;

        let remote_object_id = resp
            .result
            .object
            .object_id
            .ok_or_else(|| CdpError::msg(format!("No object Id found for {node_id:?}")))?;
        Ok(Self {
            remote_object_id,
            backend_node_id,
            node_id: Some(node_id),
            frame_id,
            session_id,
            tab,
        })
    }

    pub(crate) fn from_remote_parts(
        tab: Arc<PageInner>,
        remote_object_id: RemoteObjectId,
        backend_node_id: BackendNodeId,
        frame_id: FrameId,
        session_id: SessionId,
    ) -> Self {
        Self {
            remote_object_id,
            backend_node_id,
            node_id: None,
            frame_id,
            session_id,
            tab,
        }
    }

    /// Convert a slice of `NodeId`s into a `Vec` of `Element`s
    pub(crate) async fn from_nodes(
        tab: &Arc<PageInner>,
        node_ids: &[NodeId],
        frame_id: FrameId,
    ) -> Result<Vec<Self>> {
        let session_id = tab.session_id().clone();
        Self::from_nodes_in_session(tab, node_ids, frame_id, session_id).await
    }

    async fn from_nodes_in_session(
        tab: &Arc<PageInner>,
        node_ids: &[NodeId],
        frame_id: FrameId,
        session_id: SessionId,
    ) -> Result<Vec<Self>> {
        future::join_all(node_ids.iter().copied().map(|id| {
            Element::from_node_in_session(Arc::clone(tab), id, frame_id.clone(), session_id.clone())
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
    }

    /// Returns the first element in the document which matches the given CSS
    /// selector.
    pub async fn find_element(&self, selector: impl Into<String>) -> Result<Self> {
        let selector = selector.into();
        if let Some(node_id) = self.node_id {
            let node_id = self
                .execute(QuerySelectorParams::new(node_id, selector))
                .await?
                .result
                .node_id;
            if *node_id.inner() == 0 {
                return Err(CdpError::NotFound);
            }
            return Self::from_node_in_session(
                Arc::clone(&self.tab),
                node_id,
                self.frame_id.clone(),
                self.session_id.clone(),
            )
            .await;
        }

        let remote_object_id = self
            .query_remote_object(
                "function(sel) { return this.querySelector(sel); }",
                selector,
            )
            .await?
            .ok_or(CdpError::NotFound)?;
        self.element_from_remote_object(remote_object_id).await
    }

    /// Return all `Element`s in the document that match the given selector
    pub async fn find_elements(&self, selector: impl Into<String>) -> Result<Vec<Element>> {
        let selector = selector.into();
        if let Some(node_id) = self.node_id {
            let node_ids = self
                .execute(QuerySelectorAllParams::new(node_id, selector))
                .await?
                .result
                .node_ids;
            return Self::from_nodes_in_session(
                &self.tab,
                &node_ids,
                self.frame_id.clone(),
                self.session_id.clone(),
            )
            .await;
        }

        let Some(node_list_id) = self
            .query_remote_object(
                "function(sel) { return this.querySelectorAll(sel); }",
                selector,
            )
            .await?
        else {
            return Ok(Vec::new());
        };

        let mut params = GetPropertiesParams::new(node_list_id.clone());
        params.own_properties = Some(true);
        let properties = match self.execute(params).await {
            Ok(properties) => properties.result,
            Err(error) => {
                self.release_remote_objects_best_effort([node_list_id])
                    .await;
                return Err(error);
            }
        };
        if let Some(exception) = properties.exception_details {
            let mut cleanup = exception
                .exception
                .as_ref()
                .and_then(|exception| exception.object_id.clone())
                .into_iter()
                .collect::<Vec<_>>();
            cleanup.push(node_list_id);
            self.release_remote_objects_best_effort(cleanup).await;
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }

        let mut indexed_ids: Vec<(usize, RemoteObjectId)> = Vec::new();
        let mut missing_object_id = None;
        for property in properties.result {
            let Ok(index) = property.name.parse::<usize>() else {
                continue;
            };
            let Some(remote_object_id) = property.value.and_then(|value| value.object_id) else {
                missing_object_id.get_or_insert(property.name);
                continue;
            };
            indexed_ids.push((index, remote_object_id));
        }
        if let Some(property_name) = missing_object_id {
            let original = CdpError::msg(format!(
                "NodeList property {property_name} did not contain a remote object id"
            ));
            let mut cleanup = indexed_ids
                .iter()
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>();
            cleanup.push(node_list_id);
            self.release_remote_objects_best_effort(cleanup).await;
            return Err(original);
        }
        indexed_ids.sort_by_key(|(index, _)| *index);
        let child_ids = indexed_ids
            .into_iter()
            .map(|(_, remote_object_id)| remote_object_id)
            .collect::<Vec<_>>();

        let mut elements = Vec::with_capacity(child_ids.len());
        for remote_object_id in &child_ids {
            match self.describe_remote_object(remote_object_id.clone()).await {
                Ok(element) => elements.push(element),
                Err(error) => {
                    let mut cleanup = child_ids.clone();
                    cleanup.push(node_list_id);
                    self.release_remote_objects_best_effort(cleanup).await;
                    return Err(error);
                }
            }
        }

        // The NodeList is only a temporary enumeration container. Child
        // object ids remain alive and are owned by the returned Elements.
        self.release_remote_objects_best_effort([node_list_id])
            .await;
        Ok(elements)
    }

    async fn execute<T: Command>(&self, command: T) -> Result<CommandResponse<T::Response>> {
        self.tab
            .execute_with_session(command, &self.session_id)
            .await
    }

    async fn query_remote_object(
        &self,
        function_declaration: &str,
        selector: String,
    ) -> Result<Option<RemoteObjectId>> {
        let params = CallFunctionOnParams::builder()
            .object_id(self.remote_object_id.clone())
            .function_declaration(function_declaration)
            .argument(CallArgument::builder().value(selector).build())
            .return_by_value(false)
            .build()
            .map_err(CdpError::msg)?;
        let response = self.execute(params).await?.result;

        if let Some(exception) = response.exception_details {
            let cleanup = remote_object_ids_for_exception(&response.result, &exception);
            self.release_remote_objects_best_effort(cleanup).await;
            return Err(CdpError::JavascriptException(Box::new(exception)));
        }
        if response.result.subtype == Some(RemoteObjectSubtype::Null) {
            if let Some(remote_object_id) = response.result.object_id {
                self.release_remote_objects_best_effort([remote_object_id])
                    .await;
            }
            return Ok(None);
        }

        response
            .result
            .object_id
            .map(Some)
            .ok_or_else(|| CdpError::msg("Selector result did not contain a remote object id"))
    }

    async fn element_from_remote_object(&self, remote_object_id: RemoteObjectId) -> Result<Self> {
        match self.describe_remote_object(remote_object_id.clone()).await {
            Ok(element) => Ok(element),
            Err(error) => {
                self.release_remote_objects_best_effort([remote_object_id])
                    .await;
                Err(error)
            }
        }
    }

    async fn describe_remote_object(&self, remote_object_id: RemoteObjectId) -> Result<Self> {
        let backend_node_id = self
            .execute(
                DescribeNodeParams::builder()
                    .object_id(remote_object_id.clone())
                    .build(),
            )
            .await?
            .result
            .node
            .backend_node_id;
        Ok(Self::from_remote_parts(
            Arc::clone(&self.tab),
            remote_object_id,
            backend_node_id,
            self.frame_id.clone(),
            self.session_id.clone(),
        ))
    }

    async fn release_remote_objects_best_effort(
        &self,
        remote_object_ids: impl IntoIterator<Item = RemoteObjectId>,
    ) {
        let mut released = HashSet::new();
        for remote_object_id in remote_object_ids {
            if !released.insert(remote_object_id.clone()) {
                continue;
            }
            if let Err(error) = self
                .execute(ReleaseObjectParams::new(remote_object_id))
                .await
            {
                tracing::warn!(%error, "failed to release temporary remote object");
            }
        }
    }

    async fn box_model(&self) -> Result<BoxModel> {
        let model = self
            .execute(
                GetBoxModelParams::builder()
                    .backend_node_id(self.backend_node_id)
                    .build(),
            )
            .await?
            .result
            .model;
        Ok(BoxModel {
            content: ElementQuad::from_quad(&model.content),
            padding: ElementQuad::from_quad(&model.padding),
            border: ElementQuad::from_quad(&model.border),
            margin: ElementQuad::from_quad(&model.margin),
            width: model.width as u32,
            height: model.height as u32,
        })
    }

    async fn frame_boundary_offset(&self) -> Result<Point> {
        let snapshot = self
            .tab
            .frame_boundary_chain(self.frame_id.clone(), self.session_id.clone())
            .await?;
        let mut offset = Point::new(0.0, 0.0);

        for boundary in &snapshot {
            let owner = self
                .tab
                .execute_with_session(
                    GetFrameOwnerParams::new(boundary.child_frame_id.clone()),
                    &boundary.parent_session_id,
                )
                .await?
                .result;
            let model = self
                .tab
                .execute_with_session(
                    GetBoxModelParams::builder()
                        .backend_node_id(owner.backend_node_id)
                        .build(),
                    &boundary.parent_session_id,
                )
                .await?
                .result
                .model;
            // CDP geometry is already session-root-relative. Only the content
            // origin of an iframe that crosses into another session is added;
            // same-session ancestors are absent from the snapshot to avoid
            // double-counting their offsets.
            offset = offset + ElementQuad::from_quad(&model.content).top_left;
        }

        // Re-walk the complete topology after every owner box lookup. A swap
        // during the walk must fail before callers can dispatch input using a
        // coordinate assembled from two different frame trees.
        let current = self
            .tab
            .frame_boundary_chain(self.frame_id.clone(), self.session_id.clone())
            .await?;
        if current != snapshot {
            return Err(CdpError::FrameNotReady);
        }
        Ok(offset)
    }

    /// Returns the bounding box of the element (relative to the main frame)
    pub async fn bounding_box(&self) -> Result<BoundingBox> {
        let bounds = self.box_model().await?;
        let quad = bounds.border;
        let offset = self.frame_boundary_offset().await?;

        let local_x = quad.most_left();
        let local_y = quad.most_top();
        let width = quad.most_right() - local_x;
        let height = quad.most_bottom() - local_y;

        Ok(BoundingBox {
            x: local_x + offset.x,
            y: local_y + offset.y,
            width,
            height,
        })
    }

    /// Returns the best `Point` of this node to execute a click on.
    pub async fn clickable_point(&self) -> Result<Point> {
        let content_quads = self
            .execute(
                GetContentQuadsParams::builder()
                    .backend_node_id(self.backend_node_id)
                    .build(),
            )
            .await?;
        let point = content_quads
            .result
            .quads
            .iter()
            .filter(|q| q.inner().len() == 8)
            .map(ElementQuad::from_quad)
            .filter(|q| q.quad_area() > 1.)
            .map(|q| q.quad_center())
            .next()
            .ok_or_else(|| CdpError::msg("Node is either not visible or not an HTMLElement"))?;
        Ok(point + self.frame_boundary_offset().await?)
    }

    /// Submits a javascript function to the page and returns the evaluated
    /// result
    ///
    /// # Example get the element as JSON object
    ///
    /// ```no_run
    /// # use chromiumoxide::element::Element;
    /// # use chromiumoxide::error::Result;
    /// # async fn demo(element: Element) -> Result<()> {
    ///     let js_fn = "function() { return this; }";
    ///     let element_json = element.call_js_fn(js_fn, false).await?;
    ///     # Ok(())
    /// # }
    /// ```
    ///
    /// # Execute an async javascript function
    ///
    /// ```no_run
    /// # use chromiumoxide::element::Element;
    /// # use chromiumoxide::error::Result;
    /// # async fn demo(element: Element) -> Result<()> {
    ///     let js_fn = "async function() { return this; }";
    ///     let element_json = element.call_js_fn(js_fn, true).await?;
    ///     # Ok(())
    /// # }
    /// ```
    pub async fn call_js_fn(
        &self,
        function_declaration: impl Into<String>,
        await_promise: bool,
    ) -> Result<CallFunctionOnReturns> {
        let params = CallFunctionOnParams::builder()
            .object_id(self.remote_object_id.clone())
            .function_declaration(function_declaration)
            .generate_preview(true)
            .await_promise(await_promise)
            .build()
            .map_err(CdpError::msg)?;
        Ok(self.execute(params).await?.result)
    }

    /// Returns a JSON representation of this element.
    pub async fn json_value(&self) -> Result<serde_json::Value> {
        let element_json = self
            .call_js_fn("function() { return this; }", false)
            .await?;
        element_json.result.value.ok_or(CdpError::NotFound)
    }

    /// Calls [focus](https://developer.mozilla.org/en-US/docs/Web/API/HTMLElement/focus) on the element.
    pub async fn focus(&self) -> Result<&Self> {
        self.call_js_fn("function() { this.focus(); }", true)
            .await?;
        Ok(self)
    }

    /// Scrolls the element into view and uses a mouse event to move the mouse
    /// over the center of this element.
    pub async fn hover(&self) -> Result<&Self> {
        self.scroll_into_view().await?;
        self.tab.move_mouse(self.clickable_point().await?).await?;
        Ok(self)
    }

    /// Scrolls the element into view.
    ///
    /// Fails if the element's node is not a HTML element or is detached from
    /// the document
    pub async fn scroll_into_view(&self) -> Result<&Self> {
        let resp = self
            .call_js_fn(
                "async function() {
                if (!this.isConnected)
                    return 'Node is detached from document';
                if (this.nodeType !== Node.ELEMENT_NODE)
                    return 'Node is not of type HTMLElement';

                const visibleRatio = await new Promise(resolve => {
                    const observer = new IntersectionObserver(entries => {
                        resolve(entries[0].intersectionRatio);
                        observer.disconnect();
                    });
                    observer.observe(this);
                });

                if (visibleRatio !== 1.0)
                    this.scrollIntoView({
                        block: 'center',
                        inline: 'center',
                        behavior: 'instant'
                    });
                return false;
            }",
                true,
            )
            .await?;

        if resp.result.r#type == RemoteObjectType::String {
            let error_text = resp.result.value.unwrap().as_str().unwrap().to_string();
            return Err(CdpError::ScrollingFailed(error_text));
        }
        Ok(self)
    }

    /// This focuses the element by click on it
    ///
    /// Bear in mind that if `click()` triggers a navigation this element may be
    /// not exist anymore.
    pub async fn click(&self) -> Result<&Self> {
        let center = self.scroll_into_view().await?.clickable_point().await?;
        self.tab.click(center).await?;
        Ok(self)
    }

    /// Clicks the element using the provided [`ClickOptions`].
    ///
    /// This behaves the same as [`click()`], but allows customizing
    /// click behavior such as click count.
    ///
    /// Note that if the click triggers a navigation, this element
    /// may no longer exist afterwards.
    pub async fn click_with(&self, options: ClickOptions) -> Result<&Self> {
        let center = self.scroll_into_view().await?.clickable_point().await?;
        self.tab.click_with(center, options).await?;
        Ok(self)
    }

    /// Type the input
    ///
    /// # Example type text into an input element
    ///
    /// ```no_run
    /// # use chromiumoxide::page::Page;
    /// # use chromiumoxide::error::Result;
    /// # async fn demo(page: Page) -> Result<()> {
    ///     let element = page.find_element("input#searchInput").await?;
    ///     element.click().await?.type_str("this goes into the input field").await?;
    ///     # Ok(())
    /// # }
    /// ```
    pub async fn type_str(&self, input: impl AsRef<str>) -> Result<&Self> {
        self.tab.type_str(input).await?;
        Ok(self)
    }

    /// Presses the key.
    ///
    /// # Example type text into an input element and hit enter
    ///
    /// ```no_run
    /// # use chromiumoxide::page::Page;
    /// # use chromiumoxide::error::Result;
    /// # async fn demo(page: Page) -> Result<()> {
    ///     let element = page.find_element("input#searchInput").await?;
    ///     element.click().await?.type_str("this goes into the input field").await?
    ///          .press_key("Enter").await?;
    ///     # Ok(())
    /// # }
    /// ```
    pub async fn press_key(&self, key: impl AsRef<str>) -> Result<&Self> {
        self.tab.press_key(key).await?;
        Ok(self)
    }

    /// The description of the element's node
    pub async fn description(&self) -> Result<Node> {
        Ok(self
            .execute(
                DescribeNodeParams::builder()
                    .backend_node_id(self.backend_node_id)
                    .depth(100)
                    .build(),
            )
            .await?
            .result
            .node)
    }

    /// Attributes of the `Element` node in the form of flat array `[name1,
    /// value1, name2, value2]
    pub async fn attributes(&self) -> Result<Vec<String>> {
        let node = self.description().await?;
        Ok(node.attributes.unwrap_or_default())
    }

    /// Returns the value of the element's attribute
    pub async fn attribute(&self, attribute: impl AsRef<str>) -> Result<Option<String>> {
        let js_fn = format!(
            "function() {{ return this.getAttribute('{}'); }}",
            attribute.as_ref()
        );
        let resp = self.call_js_fn(js_fn, false).await?;
        if let Some(value) = resp.result.value {
            Ok(serde_json::from_value(value)?)
        } else {
            Ok(None)
        }
    }

    /// A `Stream` over all attributes and their values
    pub async fn iter_attributes(
        &self,
    ) -> Result<impl Stream<Item = (String, Result<Option<String>>)> + '_> {
        let attributes = self.attributes().await?;
        Ok(AttributeStream {
            attributes,
            fut: None,
            element: self,
        })
    }

    /// The inner text of this element.
    pub async fn inner_text(&self) -> Result<Option<String>> {
        self.string_property("innerText").await
    }

    /// The inner HTML of this element.
    pub async fn inner_html(&self) -> Result<Option<String>> {
        self.string_property("innerHTML").await
    }

    /// The outer HTML of this element.
    pub async fn outer_html(&self) -> Result<Option<String>> {
        self.string_property("outerHTML").await
    }

    /// Returns the string property of the element.
    ///
    /// If the property is an empty String, `None` is returned.
    pub async fn string_property(&self, property: impl AsRef<str>) -> Result<Option<String>> {
        let property = property.as_ref();
        let value = self.property(property).await?.ok_or(CdpError::NotFound)?;
        let txt: String = serde_json::from_value(value)?;
        if !txt.is_empty() {
            Ok(Some(txt))
        } else {
            Ok(None)
        }
    }

    /// Returns the javascript `property` of this element where `property` is
    /// the name of the requested property of this element.
    ///
    /// See also `Element::inner_html`.
    pub async fn property(&self, property: impl AsRef<str>) -> Result<Option<serde_json::Value>> {
        let js_fn = format!("function() {{ return this.{}; }}", property.as_ref());
        let resp = self.call_js_fn(js_fn, false).await?;
        Ok(resp.result.value)
    }

    /// Returns a map with all `PropertyDescriptor`s of this element keyed by
    /// their names
    pub async fn properties(&self) -> Result<HashMap<String, PropertyDescriptor>> {
        let mut params = GetPropertiesParams::new(self.remote_object_id.clone());
        params.own_properties = Some(true);

        let properties = self.execute(params).await?;

        Ok(properties
            .result
            .result
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect())
    }

    /// Explicitly release this element's remote object in its captured CDP
    /// session.
    ///
    /// `Element` clones share the same remote object id, so disposing one clone
    /// invalidates every clone. Dropping an Element performs no asynchronous
    /// cleanup; call this method when deterministic release is required.
    pub async fn dispose(self) -> Result<()> {
        self.execute(ReleaseObjectParams::new(self.remote_object_id.clone()))
            .await?;
        Ok(())
    }

    /// Scrolls the element into and takes a screenshot of it
    pub async fn screenshot(&self, format: CaptureScreenshotFormat) -> Result<Vec<u8>> {
        let mut bounding_box = self.scroll_into_view().await?.bounding_box().await?;
        let viewport = self.tab.layout_metrics().await?.css_layout_viewport;

        bounding_box.x += viewport.page_x as f64;
        bounding_box.y += viewport.page_y as f64;

        let clip = Viewport {
            x: viewport.page_x as f64 + bounding_box.x,
            y: viewport.page_y as f64 + bounding_box.y,
            width: bounding_box.width,
            height: bounding_box.height,
            scale: 1.,
        };

        self.tab
            .screenshot(
                CaptureScreenshotParams::builder()
                    .format(format)
                    .clip(clip)
                    .build(),
            )
            .await
    }

    /// Save a screenshot of the element and write it to `output`
    pub async fn save_screenshot(
        &self,
        format: CaptureScreenshotFormat,
        output: impl AsRef<Path>,
    ) -> Result<Vec<u8>> {
        let img = self.screenshot(format).await?;
        utils::write(output.as_ref(), &img).await?;
        Ok(img)
    }
}

pub(crate) fn remote_object_ids_for_exception(
    result: &RemoteObject,
    exception: &ExceptionDetails,
) -> Vec<RemoteObjectId> {
    let mut object_ids = Vec::new();
    if let Some(object_id) = result.object_id.clone() {
        object_ids.push(object_id);
    }
    if let Some(object_id) = exception
        .exception
        .as_ref()
        .and_then(|exception| exception.object_id.clone())
        && !object_ids.contains(&object_id)
    {
        object_ids.push(object_id);
    }
    object_ids
}

pub type AttributeValueFuture<'a> = Option<(
    String,
    Pin<Box<dyn Future<Output = Result<Option<String>>> + 'a>>,
)>;

/// Stream over all element's attributes
#[must_use = "streams do nothing unless polled"]
#[allow(missing_debug_implementations)]
pub struct AttributeStream<'a> {
    attributes: Vec<String>,
    fut: AttributeValueFuture<'a>,
    element: &'a Element,
}

impl<'a> Stream for AttributeStream<'a> {
    type Item = (String, Result<Option<String>>);

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        if pin.fut.is_none() {
            if let Some(name) = pin.attributes.pop() {
                let fut = Box::pin(pin.element.attribute(name.clone()));
                pin.fut = Some((name, fut));
            } else {
                return Poll::Ready(None);
            }
        }

        if let Some((name, mut fut)) = pin.fut.take() {
            if let Poll::Ready(res) = fut.poll_unpin(cx) {
                return Poll::Ready(Some((name, res)));
            } else {
                pin.fut = Some((name, fut));
            }
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{FutureExt, StreamExt};
    use serde_json::json;

    use chromiumoxide_cdp::cdp::browser_protocol::target::TargetId;
    use chromiumoxide_types::{CallId, Response};

    use super::*;
    use crate::handler::PageHandle;
    use crate::handler::target::{FrameBoundary, InternalTargetMessage};

    fn ok_response(result: serde_json::Value) -> Response {
        Response {
            id: CallId::new(1),
            result: Some(result),
            error: None,
        }
    }

    fn remote_object(object_id: &str) -> RemoteObject {
        RemoteObject::builder()
            .r#type(RemoteObjectType::Object)
            .object_id(RemoteObjectId::new(object_id))
            .build()
            .expect("remote object fixture is complete")
    }

    #[test]
    fn element_exception_cleanup_collects_both_distinct_object_ids() {
        let result = remote_object("result");
        let mut exception = ExceptionDetails::new(1, "boom", 0, 0);
        exception.exception = Some(remote_object("exception"));

        assert_eq!(
            remote_object_ids_for_exception(&result, &exception),
            vec![
                RemoteObjectId::new("result"),
                RemoteObjectId::new("exception")
            ]
        );
    }

    #[test]
    fn element_exception_cleanup_deduplicates_shared_object_id() {
        let result = remote_object("shared");
        let mut exception = ExceptionDetails::new(1, "boom", 0, 0);
        exception.exception = Some(remote_object("shared"));

        assert_eq!(
            remote_object_ids_for_exception(&result, &exception),
            vec![RemoteObjectId::new("shared")]
        );
    }

    #[tokio::test]
    async fn element_click_rejects_changed_boundary_snapshot_before_input_dispatch() {
        let mut page_handle =
            PageHandle::new(TargetId::new("target"), SessionId::new("child"), None);
        let element = Element::from_remote_parts(
            Arc::clone(page_handle.inner()),
            RemoteObjectId::new("element"),
            BackendNodeId::new(7),
            FrameId::new("child-frame"),
            SessionId::new("child"),
        );
        let snapshot = vec![FrameBoundary {
            child_frame_id: FrameId::new("child-frame"),
            child_session_id: SessionId::new("child"),
            parent_frame_id: FrameId::new("main-frame"),
            parent_session_id: SessionId::new("parent"),
        }];
        let mut changed_snapshot = snapshot.clone();
        changed_snapshot[0].parent_session_id = SessionId::new("new-parent");

        let controller = async {
            let InternalTargetMessage::SessionCommand {
                session_id,
                command,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("scroll command is sent")
            else {
                panic!("expected scroll session command")
            };
            assert_eq!(session_id, SessionId::new("child"));
            assert_eq!(command.method.as_ref(), CallFunctionOnParams::IDENTIFIER);
            command
                .sender
                .send(Ok(ok_response(json!({
                    "result": { "type": "boolean", "value": false }
                }))))
                .expect("scroll response receiver remains live");

            let InternalTargetMessage::SessionCommand {
                session_id,
                command,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("quad command is sent")
            else {
                panic!("expected quad session command")
            };
            assert_eq!(session_id, SessionId::new("child"));
            assert_eq!(command.method.as_ref(), GetContentQuadsParams::IDENTIFIER);
            command
                .sender
                .send(Ok(ok_response(json!({
                    "quads": [[0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0]]
                }))))
                .expect("quad response receiver remains live");

            let InternalTargetMessage::GetFrameBoundaryChain {
                frame_id,
                expected_session_id,
                tx,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("initial boundary query is sent")
            else {
                panic!("expected initial boundary query")
            };
            assert_eq!(frame_id, FrameId::new("child-frame"));
            assert_eq!(expected_session_id, SessionId::new("child"));
            tx.send(Ok(snapshot.clone()))
                .expect("initial boundary receiver remains live");

            let InternalTargetMessage::SessionCommand {
                session_id,
                command,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("frame owner command is sent")
            else {
                panic!("expected frame owner session command")
            };
            assert_eq!(session_id, SessionId::new("parent"));
            assert_eq!(command.method.as_ref(), GetFrameOwnerParams::IDENTIFIER);
            command
                .sender
                .send(Ok(ok_response(json!({ "backendNodeId": 9 }))))
                .expect("frame owner response receiver remains live");

            let InternalTargetMessage::SessionCommand {
                session_id,
                command,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("owner box command is sent")
            else {
                panic!("expected owner box session command")
            };
            assert_eq!(session_id, SessionId::new("parent"));
            assert_eq!(command.method.as_ref(), GetBoxModelParams::IDENTIFIER);
            command
                .sender
                .send(Ok(ok_response(json!({
                    "model": {
                        "content": [10.0, 20.0, 20.0, 20.0, 20.0, 30.0, 10.0, 30.0],
                        "padding": [10.0, 20.0, 20.0, 20.0, 20.0, 30.0, 10.0, 30.0],
                        "border": [10.0, 20.0, 20.0, 20.0, 20.0, 30.0, 10.0, 30.0],
                        "margin": [10.0, 20.0, 20.0, 20.0, 20.0, 30.0, 10.0, 30.0],
                        "width": 10,
                        "height": 10
                    }
                }))))
                .expect("owner box response receiver remains live");

            let InternalTargetMessage::GetFrameBoundaryChain { tx, .. } = page_handle
                .internal_rx
                .next()
                .await
                .expect("boundary re-check is sent")
            else {
                panic!("expected boundary re-check")
            };
            tx.send(Ok(changed_snapshot))
                .expect("boundary re-check receiver remains live");
        };

        let (click_result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            futures::join!(element.click(), controller)
        })
        .await
        .expect("changed topology fails without waiting for input");
        assert!(matches!(click_result, Err(CdpError::FrameNotReady)));
        assert!(
            page_handle.rx.next().now_or_never().is_none(),
            "no Input command may be sent after a failed boundary re-check"
        );
    }

    #[tokio::test]
    async fn element_dispose_releases_on_the_captured_session() {
        let mut page_handle =
            PageHandle::new(TargetId::new("target"), SessionId::new("main"), None);
        let element = Element::from_remote_parts(
            Arc::clone(page_handle.inner()),
            RemoteObjectId::new("element"),
            BackendNodeId::new(7),
            FrameId::new("child-frame"),
            SessionId::new("child"),
        );

        let controller = async {
            let InternalTargetMessage::SessionCommand {
                session_id,
                command,
            } = page_handle
                .internal_rx
                .next()
                .await
                .expect("dispose command is sent")
            else {
                panic!("expected dispose session command")
            };
            assert_eq!(session_id, SessionId::new("child"));
            assert_eq!(command.method.as_ref(), ReleaseObjectParams::IDENTIFIER);
            assert_eq!(command.params["objectId"], "element");
            command
                .sender
                .send(Ok(ok_response(json!({}))))
                .expect("dispose response receiver remains live");
        };

        let (dispose_result, ()) = futures::join!(element.dispose(), controller);
        dispose_result.expect("dispose succeeds");
    }

    #[test]
    fn element_drop_does_not_send_an_implicit_release() {
        let mut page_handle =
            PageHandle::new(TargetId::new("target"), SessionId::new("main"), None);
        let element = Element::from_remote_parts(
            Arc::clone(page_handle.inner()),
            RemoteObjectId::new("element"),
            BackendNodeId::new(7),
            FrameId::new("child-frame"),
            SessionId::new("child"),
        );
        drop(element);
        assert!(page_handle.internal_rx.next().now_or_never().is_none());
    }
}
