use chromiumoxide_cdp::cdp::{CdpEvent, CdpEventMessage};
use chromiumoxide_types::Message;

/// Shape Chrome 157 sends; `privateNetworkRequestPolicy` became `localNetworkAccessRequestPolicy`.
const EXTRA_INFO: &str = r#"{"method":"Network.requestWillBeSentExtraInfo","sessionId":"S1","params":{
  "requestId":"1502.2","associatedCookies":[],"headers":{":authority":"httpbin.org"},
  "connectTiming":{"requestTime":288055.428846},
  "clientSecurityState":{"initiatorIsSecureContext":true,"initiatorIPAddressSpace":"Loopback",
    "localNetworkAccessRequestPolicy":"PermissionBlock"},
  "siteHasCookieInOtherPartition":false}}"#;

#[test]
fn chrome_157_request_extra_info_is_not_dropped() {
    let message: Message<CdpEventMessage> =
        serde_json::from_str(EXTRA_INFO).expect("the connection must parse this event");
    let Message::Event(event) = message else {
        panic!("parsed as a response");
    };
    let CdpEvent::NetworkRequestWillBeSentExtraInfo(info) = event.params else {
        panic!("parsed as another event");
    };
    let state = serde_json::to_value(info.client_security_state).unwrap();
    assert_eq!(state["localNetworkAccessRequestPolicy"], "PermissionBlock");
}

#[test]
fn the_pre_157_field_still_parses() {
    let old = EXTRA_INFO.replace(
        "localNetworkAccessRequestPolicy",
        "privateNetworkRequestPolicy",
    );
    let Message::Event(event) = serde_json::from_str::<Message<CdpEventMessage>>(&old).unwrap()
    else {
        panic!("parsed as a response");
    };
    let CdpEvent::NetworkRequestWillBeSentExtraInfo(info) = event.params else {
        panic!("parsed as another event");
    };
    let state = serde_json::to_value(info.client_security_state).unwrap();
    assert_eq!(state["privateNetworkRequestPolicy"], "PermissionBlock");
}
