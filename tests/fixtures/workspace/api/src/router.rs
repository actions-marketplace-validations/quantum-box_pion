//! A stand-in for an axum router: the same `.route(path, method(handler))`
//! shape, without the dependency.

pub type Handler = fn(u32) -> bool;

pub struct MethodRouter(pub &'static str, pub Handler);

pub fn get(handler: Handler) -> MethodRouter {
    MethodRouter("GET", handler)
}

pub fn post(handler: Handler) -> MethodRouter {
    MethodRouter("POST", handler)
}

#[derive(Default)]
pub struct Router(pub Vec<(&'static str, MethodRouter)>);

impl Router {
    pub fn route(mut self, path: &'static str, method: MethodRouter) -> Self {
        self.0.push((path, method));
        self
    }
}

pub fn build() -> Router {
    Router::default()
        .route("/v1/auth/sign-in", post(crate::sign_in_handler))
        .route("/v1/auth/sign-up", post(crate::sign_up_handler))
        .route("/v1/health/{probe}", get(crate::health_handler))
}
