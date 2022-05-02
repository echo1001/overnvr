use std::pin::Pin;

use futures::Future;

use actix_identity::Identity;
use actix_web::{dev::Payload, web::{Query}, Error, FromRequest, HttpRequest, web::Data};
use crate::database::Database;

use super::error::ServiceError;
use hmac::{Hmac};
use sha2::Sha256;
use jwt::{VerifyWithKey};

use crate::model::{UserAuth, WSToken, WSTokenQuery};

impl FromRequest for UserAuth {
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, pl: &mut Payload) -> Self::Future {
        let key = Data::<Hmac<Sha256>>::from_request(req, pl).into_inner().unwrap().into_inner();
        let database = Data::<Database>::from_request(req, pl).into_inner().unwrap().into_inner();
        let ident = Identity::from_request(req, pl);

        Box::pin(async move {
            if let Ok(ident) = ident.await {
                if let Some(user_json) = ident.identity() {
                    if let Ok(mut user) = VerifyWithKey::verify_with_key(user_json.as_str(), &*key) {
                        if let Ok(f) = database.check_auth(&mut user).await {
                            if f {
                                return Ok(user);
                            }
                        }
                    }
                }

            }

            Err(ServiceError::Unauthorized.into())
        })

    }
}

impl FromRequest for WSToken {
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, pl: &mut Payload) -> Self::Future {
        let key = Data::<Hmac<Sha256>>::from_request(req, pl).into_inner().unwrap().into_inner();
        let database = Data::<Database>::from_request(req, pl).into_inner().unwrap().into_inner();
        let ident = Query::<WSTokenQuery>::from_request(req, pl);

        Box::pin(async move {
            if let Ok(ident) = ident.await {
                if let Ok(user) = VerifyWithKey::verify_with_key(ident.token.as_str(), &*key) {
                    if let Ok(f) = database.check_token(&user).await {
                        if f && user.scope == "websocket" {
                            return Ok(user);
                        }
                    }
                }
            }

            Err(ServiceError::Unauthorized.into())
        })

    }
}