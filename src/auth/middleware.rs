use std::future::{ready, Ready};
use std::rc::Rc;
use std::task::{Context, Poll};

use actix_service::{Service, Transform};
use actix_web::{
    body::EitherBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header,
    Error, HttpMessage, ResponseError,
};
use futures::future::LocalBoxFuture;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

use super::claims::AuthClaims;
use crate::error::ApiError;

#[derive(Clone)]
pub struct JwtAuth {
    secret: Rc<String>,
}

impl JwtAuth {
    pub fn new(secret: String) -> Self {
        Self { secret: Rc::new(secret) }
    }
}

impl<S, B> Transform<S, ServiceRequest> for JwtAuth
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = JwtAuthMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(JwtAuthMiddleware {
            service: Rc::new(service),
            secret: self.secret.clone(),
        }))
    }
}

pub struct JwtAuthMiddleware<S> {
    service: Rc<S>,
    secret: Rc<String>,
}

impl<S, B> Service<ServiceRequest> for JwtAuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let secret = self.secret.clone();
        let svc = self.service.clone();

        Box::pin(async move {
            let token_opt = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .map(|s| s.to_string());

            let token = match token_opt {
                Some(t) => t,
                None => {
                    return Ok(unauthorized_response(req, "missing or malformed Authorization header"));
                }
            };

            let mut validation = Validation::new(Algorithm::HS256);
            validation.validate_exp = true;
            // Falcon's `iss` is the user_id rendered as a string, not a fixed issuer.
            // Don't validate aud/iss.
            validation.validate_aud = false;

            let claims = match decode::<AuthClaims>(
                &token,
                &DecodingKey::from_secret(secret.as_bytes()),
                &validation,
            ) {
                Ok(data) => data.claims,
                Err(e) => {
                    return Ok(unauthorized_response(
                        req,
                        &format!("invalid token: {e}"),
                    ));
                }
            };

            req.extensions_mut().insert(claims);
            req.extensions_mut().insert(BearerToken(token));

            let res = svc.call(req).await?;
            Ok(res.map_into_left_body())
        })
    }
}

fn unauthorized_response<B>(req: ServiceRequest, msg: &str) -> ServiceResponse<EitherBody<B>> {
    let err = ApiError::Unauthorized(msg.to_string());
    let resp = err.error_response();
    let (http_req, _payload) = req.into_parts();
    ServiceResponse::new(http_req, resp).map_into_right_body()
}

/// Wrapper for the raw bearer token so handlers can relay it to Falcon.
#[derive(Clone)]
pub struct BearerToken(pub String);
