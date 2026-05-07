use std::future::{ready, Ready};

use actix_web::{dev::Payload, FromRequest, HttpMessage, HttpRequest};

use super::claims::AuthClaims;
use super::middleware::BearerToken;
use crate::error::ApiError;

impl FromRequest for AuthClaims {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let result = req
            .extensions()
            .get::<AuthClaims>()
            .cloned()
            .ok_or_else(|| {
                ApiError::Unauthorized("auth claims not present (middleware missing?)".into())
            });
        ready(result)
    }
}

impl FromRequest for BearerToken {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let result = req
            .extensions()
            .get::<BearerToken>()
            .cloned()
            .ok_or_else(|| ApiError::Unauthorized("bearer token not in request extensions".into()));
        ready(result)
    }
}
