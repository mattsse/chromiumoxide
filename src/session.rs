use crate::cdp::browser_protocol::target::SessionId;

pub struct CDPSession {
    /// `TargetInfo::r#type`
    target_type: String,
    id: SessionId,
}

fn sessionfactory() {
    // await this.send('Target.attachToTarget', {
    //     targetId: targetInfo.targetId,
    //     flatten: true,
    // });
}
