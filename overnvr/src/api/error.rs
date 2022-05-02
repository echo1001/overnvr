

use actix_web::{error::ResponseError, HttpResponse};
use serde_json::json;
use thiserror::Error;


#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("Internal Server Error")]
    InternalServerError(#[from] anyhow::Error),

    #[error("BadRequest")]
    BadRequest(String),

    #[error("Unauthorized")]
    Unauthorized,
}

// impl ResponseError trait allows to convert our errors into http responses with appropriate data
impl ResponseError for ServiceError {
    fn error_response(&self) -> HttpResponse {
        match self {
            ServiceError::InternalServerError(_) => {
                HttpResponse::InternalServerError().json("Internal Server Error, Please try later")
            }
            ServiceError::BadRequest(ref message) => HttpResponse::BadRequest().json(message),
            ServiceError::Unauthorized => HttpResponse::Unauthorized().json(json!({"error": "unauthorized"})),
        }
    }
}
