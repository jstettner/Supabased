use tonic::{Request, Response, Status};

use supabased_proto::supabased::{
    supabased_server::Supabased, WhoAmIRequest, WhoAmIResponse,
};

#[derive(Debug, Default)]
pub struct SupabasedService;

#[tonic::async_trait]
impl Supabased for SupabasedService {
    async fn who_am_i(
        &self,
        _request: Request<WhoAmIRequest>,
    ) -> Result<Response<WhoAmIResponse>, Status> {
        Ok(Response::new(WhoAmIResponse {
            identity: "anonymous".to_string(),
            permissions: vec![],
            accessible_branches: vec![],
        }))
    }
}
