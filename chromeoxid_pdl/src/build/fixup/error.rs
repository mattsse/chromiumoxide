use thiserror::Error;

#[derive(Error, Debug)]
pub enum MessageError {
    #[error("msg is missing params or result")]
    MsgMissingParamsOrResult,
}
